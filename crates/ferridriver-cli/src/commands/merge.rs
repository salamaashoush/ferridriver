//! `ferridriver merge-reports` — fold several shards' blob reports into
//! one.
//!
//! Sharded CI runs each write a `blob` zip; this replays every recorded
//! event through a fresh reporter set, so the merged HTML / `JUnit` / JSON
//! is identical to what an unsharded run would have produced. Mirrors
//! Playwright's `merge-reports` command.

use std::path::PathBuf;

use ferridriver_config::FerridriverConfig;
use ferridriver_test::config::ReporterConfig;
use ferridriver_test::reporter::{ReporterEvent, RunStatus, blob};

use crate::cli::MergeReportsArgs;
use crate::ui;

/// Run the merge. Exit code is the merged run's: non-zero when any test
/// ended unexpectedly, so a merge step can gate a pipeline the same way
/// the shards do.
///
/// # Errors
///
/// Fails when an input path holds no readable blob.
pub async fn run(config: FerridriverConfig, args: MergeReportsArgs) -> anyhow::Result<()> {
  let mut test_config = config.test;
  if let Some(dir) = args.output_dir.clone() {
    test_config.output_dir = dir;
  }

  let blobs = collect_blobs(&args.inputs)?;
  if blobs.is_empty() {
    anyhow::bail!(
      "no blob reports found in {}",
      args
        .inputs
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
    );
  }

  let mut events: Vec<ReporterEvent> = Vec::new();
  for path in &blobs {
    let read = blob::read_blob(path).map_err(|e| anyhow::anyhow!("{e}"))?;
    events.extend(read);
  }

  let names: Vec<ReporterConfig> = if args.reporter.is_empty() {
    test_config.reporter.clone()
  } else {
    args
      .reporter
      .iter()
      .map(|name| ReporterConfig {
        name: name.clone(),
        options: std::collections::BTreeMap::new(),
      })
      .collect()
  };

  // A shard's blob carries its own run boundary; the merged stream needs
  // exactly one, with the totals summed across every shard.
  let merged = merge_boundaries(events);
  let failed = merged
    .iter()
    .any(|event| matches!(event, ReporterEvent::RunFinished { failed, .. } if *failed > 0));

  let mut reporters = ferridriver_test::reporter::create_reporters_mode(
    &names,
    &test_config,
    ferridriver_test::reporter::ReporterMode::Merge,
  );
  for event in &merged {
    reporters.emit(event).await;
  }
  reporters.finalize().await;

  ui::note(&format!(
    "merged {} blob report{} from {}",
    ui::number(blobs.len()),
    if blobs.len() == 1 { "" } else { "s" },
    blobs
      .iter()
      .map(|p| ui::short_path(p, 40))
      .collect::<Vec<_>>()
      .join(", ")
  ));

  if failed {
    // The reporters just wrote the report that says what failed; the process
    // needs the status, not a second banner over the top of it.
    return Err(crate::error::AlreadyReported.into());
  }
  Ok(())
}

/// Every blob under the given paths: a directory contributes its zips,
/// a file contributes itself.
fn collect_blobs(inputs: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
  let mut out: Vec<PathBuf> = Vec::new();
  for input in inputs {
    if input.is_file() {
      out.push(input.clone());
      continue;
    }
    if !input.is_dir() {
      anyhow::bail!("{} is neither a file nor a directory", input.display());
    }
    for entry in std::fs::read_dir(input)? {
      let path = entry?.path();
      if path.extension().and_then(|e| e.to_str()) == Some("zip") {
        out.push(path);
      }
    }
  }
  // Shards are named `report-1.zip`, `report-2.zip`: sorting keeps the
  // merged report in shard order rather than directory order.
  out.sort();
  out.dedup();
  Ok(out)
}

/// Collapse the shards' `RunStarted` / `RunFinished` pairs into one,
/// summing the totals and keeping the earliest start.
fn merge_boundaries(events: Vec<ReporterEvent>) -> Vec<ReporterEvent> {
  let mut total = 0usize;
  let mut passed = 0usize;
  let mut failed = 0usize;
  let mut skipped = 0usize;
  let mut flaky = 0usize;
  let mut workers = 0u32;
  let mut duration = std::time::Duration::ZERO;
  let mut status = RunStatus::Passed;
  let mut start_time: Option<std::time::SystemTime> = None;
  let mut metadata = serde_json::Value::Null;
  let mut preamble = ferridriver_test::reporter::api::RunPreamble::empty();

  let mut body: Vec<ReporterEvent> = Vec::new();
  for event in events {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata: meta,
        start_time: started,
        preamble: shard,
      } => {
        // Each shard's tree covers only the tests it ran; the merged
        // report has to describe the whole suite.
        preamble.merge_from((*shard).clone());
        total += total_tests;
        workers += num_workers;
        if metadata.is_null() {
          metadata = meta;
        }
        start_time = Some(match start_time {
          Some(existing) => existing.min(started),
          None => started,
        });
      },
      ReporterEvent::RunFinished {
        total: shard_total,
        passed: shard_passed,
        failed: shard_failed,
        skipped: shard_skipped,
        flaky: shard_flaky,
        duration: shard_duration,
        status: shard_status,
      } => {
        // A shard whose blob lost its RunStarted still contributes its
        // own total here.
        if total < shard_total {
          total = total.max(shard_total);
        }
        passed += shard_passed;
        failed += shard_failed;
        skipped += shard_skipped;
        flaky += shard_flaky;
        // Shards run concurrently, so the merged wall time is the
        // longest shard, not the sum.
        duration = duration.max(shard_duration);
        if shard_status != RunStatus::Passed {
          status = shard_status;
        }
      },
      other => body.push(other),
    }
  }

  let mut out = Vec::with_capacity(body.len() + 2);
  out.push(ReporterEvent::RunStarted {
    total_tests: total,
    num_workers: workers,
    metadata,
    start_time: start_time.unwrap_or(std::time::SystemTime::UNIX_EPOCH),
    preamble: std::sync::Arc::new(preamble),
  });
  out.extend(body);
  out.push(ReporterEvent::RunFinished {
    total,
    passed,
    failed,
    skipped,
    flaky,
    duration,
    status,
  });
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  fn started(total: usize, workers: u32) -> ReporterEvent {
    ReporterEvent::RunStarted {
      total_tests: total,
      num_workers: workers,
      metadata: serde_json::Value::Null,
      start_time: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10),
      preamble: std::sync::Arc::new(ferridriver_test::reporter::api::RunPreamble::empty()),
    }
  }

  fn finished(total: usize, passed: usize, failed: usize, secs: u64) -> ReporterEvent {
    ReporterEvent::RunFinished {
      total,
      passed,
      failed,
      skipped: 0,
      flaky: 0,
      duration: std::time::Duration::from_secs(secs),
      status: if failed > 0 {
        RunStatus::Failed
      } else {
        RunStatus::Passed
      },
    }
  }

  #[test]
  fn two_shards_collapse_into_one_boundary() {
    let merged = merge_boundaries(vec![
      started(3, 2),
      finished(3, 3, 0, 5),
      started(4, 2),
      finished(4, 3, 1, 9),
    ]);
    assert_eq!(merged.len(), 2, "one RunStarted and one RunFinished");
    let ReporterEvent::RunStarted {
      total_tests,
      num_workers,
      ..
    } = &merged[0]
    else {
      panic!("expected RunStarted");
    };
    assert_eq!(*total_tests, 7);
    assert_eq!(*num_workers, 4);
    let ReporterEvent::RunFinished {
      total,
      passed,
      failed,
      duration,
      status,
      ..
    } = &merged[1]
    else {
      panic!("expected RunFinished");
    };
    assert_eq!(*total, 7);
    assert_eq!(*passed, 6);
    assert_eq!(*failed, 1);
    assert_eq!(*duration, std::time::Duration::from_secs(9), "shards ran concurrently");
    assert_eq!(*status, RunStatus::Failed);
  }
}
