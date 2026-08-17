//! Shared test doubles for the `ferridriver-script` integration tests.
//!
//! `MockBridge` is the [`TestHostBridge`] the invocation tests and the
//! BDD-world tests both run against: one recorder, so a change to the
//! bridge contract cannot be satisfied differently in two places.

#![allow(dead_code)]

use std::sync::Mutex;

use ferridriver_test::host::{BridgeFuture, SnapshotTarget, TestHostBridge};

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
  pub snapshot_calls: Vec<String>,
}

#[derive(Default)]
pub struct MockBridge(Mutex<MockBridgeState>);

impl MockBridge {
  pub fn state<R>(&self, f: impl FnOnce(&mut MockBridgeState) -> R) -> R {
    f(&mut self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
  }
}

impl TestHostBridge for MockBridge {
  fn attach(&self, name: String, content_type: String, body: Vec<u8>) -> BridgeFuture<()> {
    self.state(|s| s.attachments.push((name, content_type, body)));
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

  fn begin_step(&self, title: String, parent: Option<String>, _location: Option<(u32, u32)>) -> BridgeFuture<String> {
    let id = self.state(|s| {
      s.next_step_id += 1;
      let id = format!("s{}", s.next_step_id);
      s.steps.push(title.clone());
      s.step_events.push(format!(
        "begin {id} `{title}` parent={}",
        parent.as_deref().unwrap_or("-")
      ));
      id
    });
    Box::pin(async move { id })
  }

  fn end_step(&self, step_id: String, error: Option<String>) -> BridgeFuture<()> {
    self.state(|s| {
      s.step_events
        .push(format!("end {step_id} err={}", error.as_deref().unwrap_or("-")));
    });
    Box::pin(async {})
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

  fn snapshot_path(&self, name: &str) -> String {
    format!("/snap/{name}")
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
