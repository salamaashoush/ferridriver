//! NAPI `TestInfo` class — Playwright-compatible test information and runtime modifiers.
//!
//! Wraps the core `ferridriver_test::model::TestInfo` plus shared `TestModifiers`.
//! Methods like `skip()`, `fail()`, `slow()` set flags on the modifiers that the
//! Rust worker reads after the JS callback returns. All logic stays in Rust.

use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

/// Runtime test information — mirrors Playwright's `TestInfo` interface.
///
/// Properties delegate to the core Rust `TestInfo`. Modifier methods (`skip`, `fail`,
/// `slow`, `fixme`, `setTimeout`) write to shared `TestModifiers` that the Rust worker
/// reads after the test callback returns.
#[napi]
pub struct TestInfo {
  inner: Arc<ferridriver_test::model::TestInfo>,
  modifiers: Arc<ferridriver_test::model::TestModifiers>,
}

impl TestInfo {
  pub(crate) fn new(
    inner: Arc<ferridriver_test::model::TestInfo>,
    modifiers: Arc<ferridriver_test::model::TestModifiers>,
  ) -> Self {
    Self { inner, modifiers }
  }
}

#[napi]
impl TestInfo {
  // ── Properties (read-only, delegate to inner TestInfo) ──

  #[napi(getter)]
  pub fn title(&self) -> String {
    self.inner.test_id.name.clone()
  }

  #[napi(getter)]
  pub fn title_path(&self) -> Vec<String> {
    self.inner.title_path.clone()
  }

  #[napi(getter)]
  pub fn file(&self) -> String {
    self.inner.test_id.file.clone()
  }

  #[napi(getter)]
  pub fn test_id(&self) -> String {
    self.inner.test_id.full_name()
  }

  #[napi(getter)]
  pub fn tags(&self) -> Vec<String> {
    self.inner.tags.clone()
  }

  #[napi(getter)]
  pub fn retry(&self) -> i32 {
    self.inner.retry as i32
  }

  #[napi(getter)]
  pub fn worker_index(&self) -> i32 {
    self.inner.worker_index as i32
  }

  #[napi(getter)]
  pub fn parallel_index(&self) -> i32 {
    self.inner.parallel_index as i32
  }

  #[napi(getter)]
  pub fn repeat_each_index(&self) -> i32 {
    self.inner.repeat_each_index as i32
  }

  #[napi(getter)]
  pub fn output_dir(&self) -> String {
    self.inner.output_dir.display().to_string()
  }

  #[napi(getter)]
  pub fn snapshot_dir(&self) -> String {
    self.inner.snapshot_dir.display().to_string()
  }

  #[napi(getter)]
  pub fn timeout(&self) -> f64 {
    self.inner.timeout.as_millis() as f64
  }

  /// Elapsed time since test start, in milliseconds.
  #[napi(getter)]
  pub fn duration(&self) -> f64 {
    self.inner.elapsed().as_millis() as f64
  }

  /// Source line number of the test declaration.
  #[napi(getter)]
  pub fn line(&self) -> i32 {
    self.inner.test_id.line.unwrap_or(0) as i32
  }

  /// Expected status: "passed", "failed", "skipped".
  #[napi(getter, js_name = "expectedStatus")]
  pub fn expected_status(&self) -> String {
    if self.modifiers.skipped.load(std::sync::atomic::Ordering::Relaxed) {
      "skipped".to_string()
    } else if self
      .modifiers
      .expected_failure
      .load(std::sync::atomic::Ordering::Relaxed)
    {
      "failed".to_string()
    } else {
      "passed".to_string()
    }
  }

  /// Annotations as JSON array (read-only snapshot).
  #[napi(getter)]
  pub fn annotations(&self) -> Vec<serde_json::Value> {
    // Annotations are behind an async Mutex, use try_lock for sync getter.
    if let Ok(anns) = self.inner.annotations.try_lock() {
      anns.iter().filter_map(|a| serde_json::to_value(a).ok()).collect()
    } else {
      Vec::new()
    }
  }

  /// Attachments count (attachments are collected by Rust, not exposed as objects).
  #[napi(getter, js_name = "attachmentCount")]
  pub fn attachment_count(&self) -> i32 {
    if let Ok(atts) = self.inner.attachments.try_lock() {
      atts.len() as i32
    } else {
      0
    }
  }

  // ── Runtime modifiers (Playwright TestInfo methods) ──

  /// Skip this test at runtime. Mirrors Playwright's `testInfo.skip()`.
  ///
  /// If `condition` is true (or omitted), sets the skip flag and throws
  /// a sentinel error that the Rust worker catches and reports as skipped.
  #[napi]
  pub fn skip(&self, condition: Option<bool>, reason: Option<String>) -> Result<()> {
    let condition = condition.unwrap_or(true);
    if !condition {
      return Ok(());
    }
    self.modifiers.skipped.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut r) = self.modifiers.skip_reason.lock() {
      *r = reason.clone();
    }
    let msg = format!("__FERRIDRIVER_SKIP__:{}", reason.unwrap_or_default());
    Err(napi::Error::new(napi::Status::GenericFailure, msg))
  }

  /// Mark as known bug — same as skip. Mirrors Playwright's `testInfo.fixme()`.
  #[napi]
  pub fn fixme(&self, condition: Option<bool>, reason: Option<String>) -> Result<()> {
    self.skip(condition, reason)
  }

  /// Expect this test to fail (inverts pass/fail). Mirrors Playwright's `testInfo.fail()`.
  ///
  /// Does NOT throw — the test body continues running. The Rust worker reads the flag
  /// after the callback returns and applies the expected-failure inversion.
  #[napi]
  pub fn fail(&self, condition: Option<bool>, _reason: Option<String>) {
    let condition = condition.unwrap_or(true);
    if condition {
      self
        .modifiers
        .expected_failure
        .store(true, std::sync::atomic::Ordering::Relaxed);
    }
  }

  /// Triple the timeout. Mirrors Playwright's `testInfo.slow()`.
  ///
  /// Note: since the timeout is already running, this flag is read by the worker
  /// for reporting purposes and applied on retries.
  #[napi]
  pub fn slow(&self, condition: Option<bool>, _reason: Option<String>) {
    let condition = condition.unwrap_or(true);
    if condition {
      self.modifiers.slow.store(true, std::sync::atomic::Ordering::Relaxed);
    }
  }

  /// Override the test timeout at runtime. Mirrors Playwright's `testInfo.setTimeout()`.
  #[napi]
  pub fn set_timeout(&self, timeout_ms: f64) {
    if let Ok(mut t) = self.modifiers.timeout_override.lock() {
      *t = Some(timeout_ms as u64);
    }
  }

  /// Output path helper — builds a path under `outputDir` for this test.
  #[napi]
  pub fn output_path(&self, segments: Vec<String>) -> String {
    let mut path = self.inner.output_dir.clone();
    for seg in segments {
      path = path.join(seg);
    }
    path.display().to_string()
  }

  /// Snapshot path helper — builds a path under `snapshotDir` for this test.
  #[napi]
  pub fn snapshot_path(&self, segments: Vec<String>) -> String {
    let mut path = self.inner.snapshot_dir.clone();
    for seg in segments {
      path = path.join(seg);
    }
    path.display().to_string()
  }

  /// Begin a structured test step. Returns a `StepHandle` that must be completed via `end()`.
  /// Mirrors Playwright's `testInfo.step()`.
  #[napi]
  pub async fn begin_step(&self, title: String) -> crate::step_handle::StepHandle {
    let handle = self
      .inner
      .begin_step(&title, ferridriver_test::model::StepCategory::TestStep)
      .await;
    crate::step_handle::StepHandle::new(handle)
  }

  /// Add an attachment to this test. Mirrors Playwright's `testInfo.attach()`.
  ///
  /// Accepts either `body` (Buffer) or `path` (file path string) — same as Playwright.
  #[napi]
  pub async fn attach(
    &self,
    name: String,
    content_type: Option<String>,
    body: Option<napi::bindgen_prelude::Buffer>,
    path: Option<String>,
  ) {
    let ct = content_type.unwrap_or_else(|| "application/octet-stream".to_string());
    let attachment_body = if let Some(buf) = body {
      ferridriver_test::model::AttachmentBody::Bytes(buf.to_vec())
    } else if let Some(p) = path {
      ferridriver_test::model::AttachmentBody::Path(std::path::PathBuf::from(p))
    } else {
      return;
    };
    self.inner.attach(name, ct, attachment_body).await;
  }

  /// Add a structured annotation. Mirrors Playwright's `testInfo.annotations.push()`.
  #[napi]
  pub async fn annotate(&self, type_name: String, description: String) {
    self.inner.annotate(type_name, description).await;
  }
}
