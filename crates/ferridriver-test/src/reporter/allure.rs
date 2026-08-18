//! Allure 2.x reporter: writes per-test JSON results for Allure Report.
//!
//! Output format: one `{uuid}-result.json` per test case in `allure-results/`,
//! plus `environment.properties` and `categories.json`. Attachments are copied
//! as `{uuid}-attachment.{ext}` files alongside the results.
//!
//! Compatible with `allure serve allure-results` and Allure Report CI plugins.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustc_hash::FxHashMap;
use serde::Serialize;

use crate::model::{AttachmentBody, TestAnnotation, TestOutcome, TestStatus, TestStep};
use crate::reporter::{Reporter, ReporterEvent};

// ── Allure JSON schema types ──

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AllureResult {
  uuid: String,
  history_id: String,
  name: String,
  full_name: String,
  status: &'static str,
  #[serde(skip_serializing_if = "Option::is_none")]
  status_details: Option<AllureStatusDetails>,
  stage: &'static str,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<AllureStep>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  attachments: Vec<AllureAttachment>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  parameters: Vec<AllureParameter>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  labels: Vec<AllureLabel>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  links: Vec<AllureLink>,
  start: u64,
  stop: u64,
}

#[derive(Serialize)]
struct AllureStatusDetails {
  #[serde(skip_serializing_if = "Option::is_none")]
  message: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  trace: Option<String>,
}

#[derive(Serialize)]
struct AllureStep {
  name: String,
  status: &'static str,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  steps: Vec<AllureStep>,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  attachments: Vec<AllureAttachment>,
  start: u64,
  stop: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AllureAttachment {
  name: String,
  source: String,
  #[serde(rename = "type")]
  content_type: String,
}

#[derive(Serialize)]
struct AllureParameter {
  name: String,
  value: String,
}

#[derive(Serialize)]
struct AllureLabel {
  name: String,
  value: String,
}

#[derive(Serialize)]
struct AllureLink {
  name: String,
  url: String,
  #[serde(rename = "type")]
  link_type: String,
}

#[derive(Serialize)]
struct AllureCategory {
  name: String,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  #[serde(rename = "matchedStatuses")]
  matched_statuses: Vec<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  #[serde(rename = "messageRegex")]
  message_regex: Option<String>,
}

// ── Reporter ──

pub struct AllureReporter {
  output_dir: PathBuf,
  /// Optional suite title override from config.
  suite_title: Option<String>,
  /// Collected results to write in finalize.
  results: Vec<PendingResult>,
  /// Run-level environment info.
  env: BTreeMap<String, String>,
  /// Per-test start timestamps (recorded on TestStarted events).
  test_starts: FxHashMap<String, u64>,
  /// Run start timestamp (epoch ms).
  run_start: u64,
}

struct PendingResult {
  result: AllureResult,
  attachments: Vec<PendingAttachment>,
}

struct PendingAttachment {
  filename: String,
  body: AttachmentBody,
}

impl AllureReporter {
  pub fn new(output_dir: PathBuf) -> Self {
    Self {
      output_dir,
      suite_title: None,
      results: Vec::new(),
      env: BTreeMap::new(),
      test_starts: FxHashMap::default(),
      run_start: epoch_ms(),
    }
  }

  pub fn with_suite_title(mut self, title: String) -> Self {
    self.suite_title = Some(title);
    self
  }
}

#[async_trait::async_trait]
impl Reporter for AllureReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        ..
      } => {
        self.run_start = epoch_ms();
        self.env.insert("Total Tests".into(), total_tests.to_string());
        self.env.insert("Workers".into(), num_workers.to_string());
        self.env.insert("OS".into(), std::env::consts::OS.into());
        self.env.insert("Arch".into(), std::env::consts::ARCH.into());
        self.env.insert("ferridriver".into(), env!("CARGO_PKG_VERSION").into());
      },
      ReporterEvent::TestStarted { test_id, .. } => {
        self.test_starts.insert(test_id.full_name(), epoch_ms());
      },
      ReporterEvent::TestFinished { outcome, .. } => {
        self.collect_result(outcome);
      },
      ReporterEvent::RunFinished { duration, .. } => {
        self
          .env
          .insert("Duration".into(), format!("{:.1}s", duration.as_secs_f64()));
      },
      _ => {},
    }
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    std::fs::create_dir_all(&self.output_dir)?;

    // Write each test result.
    for pending in &self.results {
      let filename = format!("{}-result.json", pending.result.uuid);
      let path = self.output_dir.join(&filename);
      let json = serde_json::to_string_pretty(&pending.result)?;
      std::fs::write(&path, json)?;

      // Write attachments.
      for attach in &pending.attachments {
        let attach_path = self.output_dir.join(&attach.filename);
        match &attach.body {
          AttachmentBody::Bytes(bytes) => {
            std::fs::write(&attach_path, bytes)?;
          },
          AttachmentBody::Path(src) => {
            if src.exists() {
              std::fs::copy(src, &attach_path)?;
            }
          },
        }
      }
    }

    // Write environment.properties.
    if !self.env.is_empty() {
      let props: String = self
        .env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");
      std::fs::write(self.output_dir.join("environment.properties"), props).ok();
    }

    // Write categories.json (default error classification).
    let categories = vec![
      AllureCategory {
        name: "Test failures".into(),
        matched_statuses: vec!["failed".into()],
        message_regex: None,
      },
      AllureCategory {
        name: "Timeouts".into(),
        matched_statuses: vec!["broken".into()],
        message_regex: Some(".*timed? ?out.*".into()),
      },
      AllureCategory {
        name: "Infrastructure".into(),
        matched_statuses: vec!["broken".into()],
        message_regex: None,
      },
    ];
    let cats_json = serde_json::to_string_pretty(&categories)?;
    std::fs::write(self.output_dir.join("categories.json"), cats_json).ok();

    let count = self.results.len();
    tracing::info!(
      "Allure results written to {} ({count} tests)",
      self.output_dir.display()
    );
    Ok(())
  }
}

impl AllureReporter {
  fn collect_result(&mut self, outcome: &TestOutcome) {
    let uuid = make_uuid();
    // The attempt's own wall-clock start, which the worker records.
    // Falling back to a `TestStarted` timestamp keyed by name would
    // credit a retry with the first attempt's start, and a parallel run
    // has several attempts of one name in flight at once.
    let start_ms = match u64::try_from(outcome.start_epoch_ms()) {
      Ok(ms) if ms > 0 => ms,
      _ => self
        .test_starts
        .remove(&outcome.test_id.full_name())
        .unwrap_or(self.run_start),
    };
    let stop_ms = start_ms + outcome.duration.as_millis() as u64;

    let status = map_status(&outcome.status);
    let status_details = outcome.error.as_ref().map(|e| AllureStatusDetails {
      message: Some(e.message.clone()),
      trace: e.stack.clone(),
    });

    // Convert steps.
    let steps = convert_steps(&outcome.steps, start_ms);

    // Convert attachments.
    let mut allure_attachments = Vec::new();
    let mut pending_attachments = Vec::new();
    for attach in &outcome.attachments {
      let ext = mime_to_ext(&attach.content_type);
      let attach_uuid = make_uuid();
      let filename = format!("{attach_uuid}-attachment.{ext}");
      allure_attachments.push(AllureAttachment {
        name: attach.name.clone(),
        source: filename.clone(),
        content_type: attach.content_type.clone(),
      });
      pending_attachments.push(PendingAttachment {
        filename,
        body: attach.body.clone(),
      });
    }

    // Also handle screenshot-on-failure embedded in the error.
    if let Some(ref err) = outcome.error
      && let Some(ref screenshot) = err.screenshot
    {
      let attach_uuid = make_uuid();
      let filename = format!("{attach_uuid}-attachment.png");
      allure_attachments.push(AllureAttachment {
        name: "Screenshot on failure".into(),
        source: filename.clone(),
        content_type: "image/png".into(),
      });
      pending_attachments.push(PendingAttachment {
        filename,
        body: AttachmentBody::Bytes(screenshot.clone()),
      });
    }

    // Build labels from annotations.
    let suite_value = self
      .suite_title
      .clone()
      .or_else(|| outcome.test_id.suite.clone())
      .unwrap_or_default();
    let mut labels = vec![
      AllureLabel {
        name: "suite".into(),
        value: suite_value,
      },
      AllureLabel {
        name: "parentSuite".into(),
        value: outcome.test_id.file.clone(),
      },
    ];
    let mut links = Vec::new();

    for annotation in &outcome.annotations {
      match annotation {
        TestAnnotation::Tag(tag) => {
          labels.push(AllureLabel {
            name: "tag".into(),
            value: tag.clone(),
          });
        },
        TestAnnotation::Info { type_name, description } => match type_name.as_str() {
          "severity" => labels.push(AllureLabel {
            name: "severity".into(),
            value: description.clone(),
          }),
          "owner" => labels.push(AllureLabel {
            name: "owner".into(),
            value: description.clone(),
          }),
          "epic" => labels.push(AllureLabel {
            name: "epic".into(),
            value: description.clone(),
          }),
          "feature" => labels.push(AllureLabel {
            name: "feature".into(),
            value: description.clone(),
          }),
          "story" => labels.push(AllureLabel {
            name: "story".into(),
            value: description.clone(),
          }),
          "issue" => links.push(AllureLink {
            name: description.clone(),
            url: description.clone(),
            link_type: "issue".into(),
          }),
          "tms" => links.push(AllureLink {
            name: description.clone(),
            url: description.clone(),
            link_type: "tms".into(),
          }),
          _ => labels.push(AllureLabel {
            name: type_name.clone(),
            value: description.clone(),
          }),
        },
        TestAnnotation::Slow { .. } => {
          labels.push(AllureLabel {
            name: "tag".into(),
            value: "slow".into(),
          });
        },
        TestAnnotation::Fixme { reason, .. } => {
          labels.push(AllureLabel {
            name: "tag".into(),
            value: "fixme".into(),
          });
          if let Some(r) = reason {
            labels.push(AllureLabel {
              name: "description".into(),
              value: r.clone(),
            });
          }
        },
        TestAnnotation::Fail { .. } => {
          labels.push(AllureLabel {
            name: "tag".into(),
            value: "expected-failure".into(),
          });
        },
        _ => {},
      }
    }

    // Flaky label.
    if outcome.status == TestStatus::Flaky {
      labels.push(AllureLabel {
        name: "tag".into(),
        value: "flaky".into(),
      });
    }

    // Parameters: attempt info if retried.
    let mut parameters = Vec::new();
    if outcome.max_attempts > 1 {
      parameters.push(AllureParameter {
        name: "attempt".into(),
        value: format!("{}/{}", outcome.attempt, outcome.max_attempts),
      });
    }

    // Stable history ID for Allure trend tracking.
    let history_id = format!("{:x}", simple_hash(&outcome.test_id.full_name()));

    let result = AllureResult {
      uuid: uuid.clone(),
      history_id,
      name: outcome.test_id.name.clone(),
      full_name: outcome.test_id.full_name(),
      status,
      status_details,
      stage: "finished",
      steps,
      attachments: allure_attachments,
      parameters,
      labels,
      links,
      start: start_ms,
      stop: stop_ms,
    };

    self.results.push(PendingResult {
      result,
      attachments: pending_attachments,
    });
  }
}

// ── Helpers ──

fn convert_steps(steps: &[TestStep], parent_start: u64) -> Vec<AllureStep> {
  let mut offset = parent_start;
  steps
    .iter()
    .map(|s| {
      let start = offset;
      let stop = start + s.duration.as_millis() as u64;
      offset = stop;
      AllureStep {
        name: s.title.clone(),
        status: map_step_status(s),
        steps: convert_steps(&s.steps, start),
        attachments: Vec::new(),
        start,
        stop,
      }
    })
    .collect()
}

fn map_status(status: &TestStatus) -> &'static str {
  match status {
    TestStatus::Passed | TestStatus::Flaky => "passed",
    TestStatus::Failed => "failed",
    TestStatus::TimedOut | TestStatus::Interrupted => "broken",
    TestStatus::Skipped => "skipped",
  }
}

fn map_step_status(step: &TestStep) -> &'static str {
  match step.status {
    crate::model::StepStatus::Passed => "passed",
    crate::model::StepStatus::Failed => "failed",
    crate::model::StepStatus::Skipped => "skipped",
    crate::model::StepStatus::Pending => "skipped",
  }
}

fn mime_to_ext(content_type: &str) -> &str {
  match content_type {
    "image/png" => "png",
    "image/jpeg" | "image/jpg" => "jpg",
    "text/plain" => "txt",
    "text/html" => "html",
    "application/json" => "json",
    "video/webm" => "webm",
    "application/zip" => "zip",
    _ => "bin",
  }
}

/// Simple non-cryptographic hash for stable history IDs.
fn simple_hash(s: &str) -> u64 {
  let mut hash: u64 = 5381;
  for b in s.bytes() {
    hash = hash.wrapping_mul(33).wrapping_add(u64::from(b));
  }
  hash
}

/// Generate a UUID-v4-like string (no external dep, good enough for Allure).
fn make_uuid() -> String {
  use std::sync::atomic::{AtomicU64, Ordering};
  static COUNTER: AtomicU64 = AtomicU64::new(0);

  let ts = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or(Duration::ZERO)
    .as_nanos() as u64;
  let count = COUNTER.fetch_add(1, Ordering::Relaxed);

  // Mix timestamp + counter for uniqueness.
  let a = ts ^ (count.wrapping_mul(0x517c_c1b7_2722_0a95));
  let b = ts.wrapping_mul(0x6c62_272e_07bb_0142) ^ count;

  format!(
    "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
    (a >> 32) as u32,
    (a >> 16) as u16,
    a as u16 & 0x0fff,
    ((b >> 48) as u16 & 0x3fff) | 0x8000,
    b & 0xffff_ffff_ffff,
  )
}

fn epoch_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or(Duration::ZERO)
    .as_millis() as u64
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use std::path::Path;

  use crate::model::{StepCategory, StepStatus, TestFailure, TestId, TestOutcome};
  use crate::reporter::{ReporterEvent, RunStatus};

  struct ScopedDir(PathBuf);
  impl Drop for ScopedDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn scoped(name: &str) -> ScopedDir {
    let path = std::env::temp_dir().join(format!("ferri-allure-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    ScopedDir(path)
  }

  fn outcome(name: &str, status: TestStatus) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: TestId {
        file: "tests/checkout.spec.ts".into(),
        suite: Some("Checkout".into()),
        name: name.into(),
        line: Some(5),
        column: Some(1),
      },
      status,
      duration: Duration::from_millis(250),
      // 2023-11-14T22:13:20Z
      start_time: std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
      annotations: vec![
        TestAnnotation::Tag("@smoke".into()),
        TestAnnotation::Info {
          type_name: "severity".into(),
          description: "critical".into(),
        },
        TestAnnotation::Info {
          type_name: "issue".into(),
          description: "JIRA-7".into(),
        },
      ],
      ..Default::default()
    })
  }

  /// Drive a run and return the parsed `*-result.json` documents.
  async fn results(dir: &Path, outcomes: Vec<Arc<TestOutcome>>) -> Vec<serde_json::Value> {
    let mut reporter = AllureReporter::new(dir.to_path_buf());
    reporter
      .on_event(&ReporterEvent::RunStarted {
        total_tests: outcomes.len(),
        num_workers: 1,
        metadata: serde_json::Value::Null,
        start_time: std::time::SystemTime::UNIX_EPOCH,
        preamble: std::sync::Arc::new(crate::reporter::api::RunPreamble::empty()),
      })
      .await;
    for outcome in outcomes {
      reporter.on_event(&ReporterEvent::TestFinished { outcome }).await;
    }
    reporter
      .on_event(&ReporterEvent::RunFinished {
        total: 1,
        passed: 1,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration: Duration::from_millis(400),
        status: RunStatus::Passed,
      })
      .await;
    reporter.finalize().await.expect("finalize");

    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read dir") {
      let path = entry.expect("entry").path();
      if path.to_string_lossy().ends_with("-result.json") {
        let text = std::fs::read_to_string(&path).expect("read result");
        out.push(serde_json::from_str(&text).expect("parse result"));
      }
    }
    out
  }

  #[tokio::test]
  async fn a_result_carries_the_attempts_own_wall_clock_window() {
    let dir = scoped("times");
    let results = results(&dir.0, vec![outcome("pays", TestStatus::Passed)]).await;
    assert_eq!(results.len(), 1);
    let r = &results[0];
    assert_eq!(r["status"], "passed");
    assert_eq!(r["name"], "pays");
    // The window comes from the outcome, not from a name-keyed
    // TestStarted timestamp — that mis-attributed retries and parallel runs.
    assert_eq!(r["start"], 1_700_000_000_000_u64);
    assert_eq!(r["stop"], 1_700_000_000_250_u64, "start plus the attempt's duration");
  }

  #[tokio::test]
  async fn annotations_become_labels_and_links() {
    let dir = scoped("labels");
    let results = results(&dir.0, vec![outcome("pays", TestStatus::Passed)]).await;
    let labels = &results[0]["labels"];
    let has = |name: &str, value: &str| {
      labels
        .as_array()
        .expect("labels")
        .iter()
        .any(|l| l["name"] == name && l["value"] == value)
    };
    assert!(has("suite", "Checkout"), "{labels}");
    assert!(has("parentSuite", "tests/checkout.spec.ts"), "{labels}");
    assert!(has("tag", "@smoke"), "{labels}");
    assert!(has("severity", "critical"), "{labels}");
    assert_eq!(results[0]["links"][0]["type"], "issue");
    assert_eq!(results[0]["links"][0]["name"], "JIRA-7");
  }

  #[tokio::test]
  async fn a_failure_carries_its_message_trace_and_steps() {
    let dir = scoped("failure");
    let mut failed = (*outcome("pays", TestStatus::Failed)).clone();
    failed.error = Some(TestFailure {
      message: "card declined".into(),
      stack: Some("at pay (tests/checkout.spec.ts:9:1)".into()),
      diff: None,
      screenshot: Some(vec![1, 2, 3]),
    });
    failed.steps = vec![TestStep {
      step_id: "s1".into(),
      title: "enter card".into(),
      category: StepCategory::TestStep,
      duration: Duration::from_millis(20),
      status: StepStatus::Failed,
      error: Some("declined".into()),
      location: None,
      annotations: Vec::new(),
      parent_step_id: None,
      metadata: None,
      steps: Vec::new(),
    }];

    let results = results(&dir.0, vec![Arc::new(failed)]).await;
    let r = &results[0];
    assert_eq!(r["status"], "failed");
    assert_eq!(r["statusDetails"]["message"], "card declined");
    assert!(r["statusDetails"]["trace"].as_str().is_some_and(|t| t.contains("pay")));
    assert_eq!(r["steps"][0]["name"], "enter card");
    assert_eq!(r["steps"][0]["status"], "failed");

    // The failure screenshot is written beside the result, not just named.
    let source = r["attachments"][0]["source"].as_str().expect("attachment source");
    assert!(dir.0.join(source).exists(), "attachment file missing: {source}");
  }

  #[tokio::test]
  async fn a_timeout_is_broken_rather_than_failed() {
    let dir = scoped("timeout");
    let results = results(&dir.0, vec![outcome("pays", TestStatus::TimedOut)]).await;
    assert_eq!(
      results[0]["status"], "broken",
      "Allure separates a broken run from a failed assertion"
    );
  }

  #[tokio::test]
  async fn every_attempt_of_a_retried_test_is_its_own_result() {
    let dir = scoped("retries");
    let mut first = (*outcome("pays", TestStatus::Failed)).clone();
    first.attempt = 1;
    let mut second = (*outcome("pays", TestStatus::Passed)).clone();
    second.attempt = 2;
    let results = results(&dir.0, vec![Arc::new(first), Arc::new(second)]).await;
    assert_eq!(results.len(), 2, "Allure history needs both attempts");
    let history: std::collections::HashSet<&str> = results
      .iter()
      .map(|r| r["historyId"].as_str().unwrap_or_default())
      .collect();
    assert_eq!(history.len(), 1, "and they share one history id");
  }
}
