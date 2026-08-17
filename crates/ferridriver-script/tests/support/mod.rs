//! Shared test doubles for the `ferridriver-script` integration tests.
//!
//! `MockBridge` is the [`TestHostBridge`] the invocation tests and the
//! BDD-world tests both run against: one recorder, so a change to the
//! bridge contract cannot be satisfied differently in two places.

#![allow(dead_code)]

use std::sync::Mutex;

use ferridriver_test::host::{BridgeFuture, SnapshotTarget, TestHostBridge};
use ferridriver_test::step::{StepFrame, StepFuture, StepOutcome, StepSpec, StepStarted};

#[derive(Default)]
pub struct MockBridgeState {
  pub attachments: Vec<(String, String, Vec<u8>)>,
  pub annotations: Vec<(String, Option<String>)>,
  pub steps: Vec<String>,
  pub step_events: Vec<String>,
  pub soft_errors: Vec<String>,
  pub skipped: bool,
  pub skip_reason: Option<String>,
  pub expected_failure: bool,
  pub slow: bool,
  pub timeout_override: Option<u64>,
  pub next_step_id: u32,
  /// Ids of the steps open right now, outermost first.
  pub open: Vec<String>,
  pub open_titles: Vec<String>,
  pub boxed: Vec<Vec<ferridriver_test::model::StepLocation>>,
  pub step_annotations: Vec<String>,
  pub step_attachments: Vec<String>,
  pub snapshot_calls: Vec<String>,
}

#[derive(Default)]
pub struct MockBridge(Mutex<MockBridgeState>);

impl MockBridge {
  pub fn state<R>(&self, f: impl FnOnce(&mut MockBridgeState) -> R) -> R {
    f(&mut self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
  }
}

/// The recorder's step driver: same rules as the runner's (they are
/// core's), only the sink is a string log.
impl ferridriver_test::step::StepDriver for MockBridge {
  fn begin_step(&self, spec: StepSpec) -> StepFuture<'_, StepStarted> {
    let frames: Vec<ferridriver_test::model::StepLocation> = spec
      .frames
      .iter()
      .map(|f| match f {
        StepFrame::Source(loc) => loc.clone(),
        StepFrame::Host { line, column } => ferridriver_test::model::StepLocation {
          file: "bundle.js".to_string(),
          line: *line,
          column: *column,
        },
      })
      .collect();
    let started = self.state(|s| {
      s.next_step_id += 1;
      let step_id = format!("s{}", s.next_step_id);
      let parent = s.open.last().cloned();
      let (location, boxed_stack) = ferridriver_test::step::resolve_location(
        &spec.options,
        &frames,
        s.boxed.last().map_or(&[][..], |b| b.as_slice()),
      );
      s.steps.push(spec.title.clone());
      s.step_events.push(format!(
        "begin {step_id} `{}` parent={} at={}",
        spec.title,
        parent.as_deref().unwrap_or("-"),
        location.as_ref().map_or_else(|| "-".to_string(), ToString::to_string)
      ));
      let mut title_path: Vec<String> = s.open_titles.clone();
      title_path.push(spec.title.clone());
      s.open.push(step_id.clone());
      s.open_titles.push(spec.title);
      s.boxed.push(boxed_stack.clone());
      StepStarted {
        step_id,
        location,
        boxed_stack,
        title_path,
      }
    });
    Box::pin(async move { started })
  }

  fn end_step(&self, step_id: String, outcome: StepOutcome) -> StepFuture<'_, ()> {
    self.state(|s| {
      if let Some(at) = s.open.iter().position(|id| *id == step_id) {
        s.open.remove(at);
        s.open_titles.remove(at);
        s.boxed.remove(at);
      }
      s.step_events.push(format!(
        "end {step_id} err={} status={:?}",
        outcome.error.as_deref().unwrap_or("-"),
        outcome.status
      ));
      s.step_annotations
        .extend(outcome.annotations.iter().map(|a| format!("{a:?}")));
    });
    Box::pin(async {})
  }
}

impl TestHostBridge for MockBridge {
  fn attach(&self, name: String, content_type: String, body: Vec<u8>, step_id: Option<String>) -> BridgeFuture<()> {
    self.state(|s| {
      s.attachments.push((name, content_type, body));
      if let Some(step_id) = step_id {
        s.step_attachments.push(step_id);
      }
    });
    Box::pin(async {})
  }

  fn attachment_count(&self) -> usize {
    self.state(|s| s.attachments.len())
  }

  fn annotate(&self, kind: String, description: Option<String>) {
    self.state(|s| s.annotations.push((kind, description)));
  }

  fn annotations(&self) -> Vec<(String, Option<String>)> {
    self.state(|s| s.annotations.clone())
  }

  fn record_soft_error(&self, message: String, _diff: Option<String>) {
    self.state(|s| s.soft_errors.push(message));
  }

  fn set_skip(&self, reason: Option<String>) {
    self.state(|s| {
      s.skipped = true;
      s.skip_reason = reason;
    });
  }

  fn set_expected_failure(&self) {
    self.state(|s| s.expected_failure = true);
  }

  fn set_slow(&self) {
    self.state(|s| s.slow = true);
  }

  fn set_timeout_override(&self, ms: u64) {
    self.state(|s| s.timeout_override = Some(ms));
  }

  fn output_path(&self, parts: &[String]) -> String {
    format!("/out/{}", parts.join("/"))
  }

  fn snapshot_path(&self, name: &[String], kind: &str) -> Result<String, String> {
    Ok(format!("/snap/{kind}/{}", name.join("/")))
  }

  fn errors(&self) -> Vec<String> {
    self.state(|s| s.soft_errors.clone())
  }

  fn match_text_snapshot(&self, target: SnapshotTarget, name: Option<String>) -> BridgeFuture<Result<(), String>> {
    let kind = snapshot_target_kind(&target);
    self.state(|s| {
      s.snapshot_calls
        .push(format!("text {kind} name={}", name.as_deref().unwrap_or("<auto>")));
    });
    Box::pin(async { Ok(()) })
  }

  fn match_screenshot(
    &self,
    target: SnapshotTarget,
    name: Option<String>,
    options: serde_json::Value,
  ) -> BridgeFuture<Result<(), String>> {
    let kind = snapshot_target_kind(&target);
    self.state(|s| {
      s.snapshot_calls.push(format!(
        "screenshot {kind} name={} opts={options}",
        name.as_deref().unwrap_or("<auto>")
      ));
    });
    Box::pin(async { Ok(()) })
  }

  fn match_aria_snapshot(
    &self,
    target: SnapshotTarget,
    expected_yaml: String,
    is_not: bool,
    _timeout_ms: Option<u64>,
  ) -> BridgeFuture<Result<(), String>> {
    let kind = snapshot_target_kind(&target);
    self.state(|s| {
      s.snapshot_calls
        .push(format!("aria {kind} not={is_not} yaml={expected_yaml}"));
    });
    Box::pin(async { Ok(()) })
  }
}

pub fn snapshot_target_kind(target: &SnapshotTarget) -> &'static str {
  match target {
    SnapshotTarget::Locator(_) => "locator",
    SnapshotTarget::Page(_) => "page",
    SnapshotTarget::Value(_) => "value",
  }
}
