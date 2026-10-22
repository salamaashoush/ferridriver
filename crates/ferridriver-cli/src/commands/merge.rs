//! `ferridriver merge-reports` — fold several shards' blob reports (or
//! `stream` reporter output) into one.
//!
//! Sharded CI runs each write a `blob` zip; this replays every recorded
//! event through a fresh reporter set, so the merged HTML / `JUnit` / JSON
//! is identical to what an unsharded run would have produced. A test with
//! attempts in several inputs becomes one test with every attempt, in the
//! order they ran. Mirrors Playwright's `merge-reports` command.

use std::path::PathBuf;

use ferridriver_config::FerridriverConfig;
use ferridriver_test::config::ReporterConfig;
use ferridriver_test::reporter::{ReporterEvent, RunStatus, blob, merge};

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
  let caps = crate::commands::suite::caps(&config);
  let mut test_config = config.test;
  if let Some(dir) = args.output_dir.clone() {
    test_config.output_dir = dir;
  }

  let inputs = collect_inputs(&args.inputs)?;
  if inputs.is_empty() {
    return Err(crate::error::usage(format!(
      "no blob reports or event streams found in {}",
      args
        .inputs
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
    )));
  }

  let mut runs: Vec<Vec<ReporterEvent>> = Vec::with_capacity(inputs.len());
  for path in &inputs {
    let read = if is_event_stream(path) {
      blob::read_event_stream(path)
    } else {
      blob::read_blob(path)
    };
    runs.push(read.map_err(|e| anyhow::anyhow!("{e}"))?);
  }

  if !args.reporter.is_empty() {
    test_config.reporter = args
      .reporter
      .iter()
      .map(|spec| match spec.split_once('=') {
        Some((name, target)) if !name.is_empty() => ReporterConfig {
          name: name.to_string(),
          options: [("outputFile".to_string(), serde_json::Value::String(target.to_string()))]
            .into_iter()
            .collect(),
        },
        _ => ReporterConfig {
          name: spec.clone(),
          options: std::collections::BTreeMap::new(),
        },
      })
      .collect();
  }
  // A JS reporter replays a merged run as it would a live one.
  crate::commands::bootstrap::install_js_reporters(&test_config, &caps).await?;

  // Attempts of one test written by different inputs fold into one test,
  // and the totals are recomputed from the folded attempts.
  let merged = merge::merge_runs(runs);
  let failed = merged
    .iter()
    .any(|event| matches!(event, ReporterEvent::RunFinished { status, .. } if *status == RunStatus::Failed));

  let mut reporters = ferridriver_test::reporter::create_reporters_mode(
    &test_config.reporter,
    &test_config,
    ferridriver_test::reporter::ReporterMode::Merge,
  );
  for event in &merged {
    reporters.emit(event).await;
  }
  reporters.finalize().await;

  ui::note(&format!(
    "merged {} report{} from {}",
    ui::number(inputs.len()),
    if inputs.len() == 1 { "" } else { "s" },
    inputs
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

/// An event stream the `stream` reporter wrote, as opposed to a blob zip.
fn is_event_stream(path: &std::path::Path) -> bool {
  matches!(path.extension().and_then(|e| e.to_str()), Some("jsonl" | "ndjson"))
}

/// Every input under the given paths: a directory contributes its blob
/// zips and stream files, a file contributes itself.
fn collect_inputs(inputs: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
  let mut out: Vec<PathBuf> = Vec::new();
  for input in inputs {
    if input.is_file() {
      out.push(input.clone());
      continue;
    }
    if !input.is_dir() {
      return Err(crate::error::usage(format!(
        "{} is neither a file nor a directory",
        input.display()
      )));
    }
    for entry in std::fs::read_dir(input)? {
      let path = entry?.path();
      if path.extension().and_then(|e| e.to_str()) == Some("zip") || is_event_stream(&path) {
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_directory_contributes_its_blobs_and_streams_in_order() {
    let dir = std::env::temp_dir().join(format!("ferri-merge-inputs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    for name in ["report-2.zip", "report-1.zip", "worker-a.jsonl", "notes.txt"] {
      std::fs::write(dir.join(name), "").expect("file");
    }
    let found: Vec<String> = collect_inputs(std::slice::from_ref(&dir))
      .expect("inputs")
      .iter()
      .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
      .collect();
    assert_eq!(found, ["report-1.zip", "report-2.zip", "worker-a.jsonl"]);
    std::fs::remove_dir_all(&dir).ok();
  }
}
