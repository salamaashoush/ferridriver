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

  async fn finalize(&mut self) -> Result<(), String> {
    std::fs::create_dir_all(&self.output_dir).map_err(|e| format!("cannot create allure output dir: {e}"))?;

    // Write each test result.
    for pending in &self.results {
      let filename = format!("{}-result.json", pending.result.uuid);
      let path = self.output_dir.join(&filename);
      let json = serde_json::to_string_pretty(&pending.result).map_err(|e| format!("allure serialize error: {e}"))?;
      std::fs::write(&path, json).map_err(|e| format!("cannot write {}: {e}", path.display()))?;

      // Write attachments.
      for attach in &pending.attachments {
        let attach_path = self.output_dir.join(&attach.filename);
        match &attach.body {
          AttachmentBody::Bytes(bytes) => {
            std::fs::write(&attach_path, bytes)
              .map_err(|e| format!("cannot write attachment {}: {e}", attach_path.display()))?;
          },
          AttachmentBody::Path(src) => {
            if src.exists() {
              std::fs::copy(src, &attach_path)
                .map_err(|e| format!("cannot copy attachment {}: {e}", attach_path.display()))?;
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
    let cats_json =
      serde_json::to_string_pretty(&categories).map_err(|e| format!("allure categories serialize: {e}"))?;
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
    let start_ms = self
      .test_starts
      .remove(&outcome.test_id.full_name())
      .unwrap_or(self.run_start);
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
    if let Some(ref err) = outcome.error {
      if let Some(ref screenshot) = err.screenshot {
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
        TestAnnotation::Slow => {
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
        TestAnnotation::Fail => {
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
