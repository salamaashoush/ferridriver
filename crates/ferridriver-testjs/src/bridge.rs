//! [`TestHostBridge`] implementation over the core `TestInfo` /
//! `TestModifiers` — the seam a running JS test reaches the runner
//! through (`testInfo.*`, `test.step`, runtime modifiers).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferridriver_script::{BridgeFuture, CompiledBundle, TestHostBridge};
use ferridriver_test::model::{
  AttachmentBody, StepCategory, StepHandle, StepLocation, TestAnnotation, TestInfo, TestModifiers,
};

use crate::JsTestSession;

pub struct InfoBridge {
  test_info: Arc<TestInfo>,
  modifiers: Arc<TestModifiers>,
  session: Arc<JsTestSession>,
  bundle: Arc<CompiledBundle>,
  cwd: Arc<PathBuf>,
  /// Base per-test timeout — `test.slow()` re-arms the VM deadline to
  /// three times this (the worker applies the same multiplier to its
  /// own budget).
  base_timeout: Duration,
  /// Live step handles keyed by step id (`test.step` nesting). `Arc`
  /// so the async bridge futures own their handle map access.
  steps: Arc<Mutex<rustc_hash::FxHashMap<String, StepHandle>>>,
  /// Runtime annotations mirror (sync-readable for the `testInfo`
  /// getter; flushed into `TestInfo` after the body settles).
  annotations: Mutex<Vec<(String, Option<String>)>>,
  /// Registration-time annotations shown by the getter alongside the
  /// runtime ones.
  static_annotations: Vec<(String, Option<String>)>,
  attachment_count: AtomicUsize,
  soft_errors: Mutex<Vec<String>>,
}

impl InfoBridge {
  pub fn new(
    test_info: Arc<TestInfo>,
    modifiers: Arc<TestModifiers>,
    session: Arc<JsTestSession>,
    bundle: Arc<CompiledBundle>,
    cwd: Arc<PathBuf>,
    base_timeout: Duration,
    static_annotations: Vec<(String, Option<String>)>,
  ) -> Self {
    Self {
      test_info,
      modifiers,
      session,
      bundle,
      cwd,
      base_timeout,
      steps: Arc::new(Mutex::new(rustc_hash::FxHashMap::default())),
      annotations: Mutex::new(Vec::new()),
      static_annotations,
      attachment_count: AtomicUsize::new(0),
      soft_errors: Mutex::new(Vec::new()),
    }
  }

  fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Flush runtime annotations into the core `TestInfo` so reporters
  /// see them. Called by the test closure after the body settles (the
  /// bridge's own methods are sync; `TestInfo.annotations` is async).
  pub async fn flush(&self) {
    let drained: Vec<(String, Option<String>)> = Self::lock(&self.annotations).drain(..).collect();
    for (type_name, description) in drained {
      self
        .test_info
        .annotate(type_name, description.unwrap_or_default())
        .await;
    }
    // Close any step left open by a mid-step failure so reporters and
    // the trace never see a dangling span.
    let open: Vec<(String, StepHandle)> = self
      .steps
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .drain()
      .collect();
    for (_, handle) in open {
      handle
        .end(Some("step never completed (test aborted)".to_string()))
        .await;
    }
  }

  fn remap_location(&self, location: Option<(u32, u32)>) -> Option<StepLocation> {
    let (line, col) = location?;
    let (src, src_line, src_col) = self.bundle.remap(line, col)?;
    let file = {
      let p = std::path::Path::new(&src);
      let abs = if p.is_absolute() {
        p.to_path_buf()
      } else {
        self.cwd.join(p)
      };
      abs
        .strip_prefix(self.cwd.as_path())
        .map_or_else(|_| abs.display().to_string(), |r| r.display().to_string())
    };
    Some(StepLocation {
      file,
      line: src_line,
      column: src_col,
    })
  }
}

impl TestHostBridge for InfoBridge {
  fn attach(&self, name: String, content_type: String, body: Vec<u8>) -> BridgeFuture<()> {
    let info = Arc::clone(&self.test_info);
    self.attachment_count.fetch_add(1, Ordering::Relaxed);
    Box::pin(async move {
      info.attach(name, content_type, AttachmentBody::Bytes(body)).await;
    })
  }

  fn attachment_count(&self) -> usize {
    self.attachment_count.load(Ordering::Relaxed)
  }

  fn annotate(&self, kind: String, description: Option<String>) {
    Self::lock(&self.annotations).push((kind, description));
  }

  fn annotations(&self) -> Vec<(String, Option<String>)> {
    let mut out = self.static_annotations.clone();
    out.extend(Self::lock(&self.annotations).iter().cloned());
    out
  }

  fn begin_step(&self, title: String, parent: Option<String>, location: Option<(u32, u32)>) -> BridgeFuture<String> {
    let info = Arc::clone(&self.test_info);
    let location = self.remap_location(location);
    let steps = Arc::clone(&self.steps);
    Box::pin(async move {
      let handle = match &parent {
        Some(p) => info.begin_child_step(title, StepCategory::TestStep, p).await,
        None => info.begin_step_at(title, StepCategory::TestStep, location).await,
      };
      let id = handle.step_id.clone();
      steps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(id.clone(), handle);
      id
    })
  }

  fn end_step(&self, step_id: String, error: Option<String>) -> BridgeFuture<()> {
    let steps = Arc::clone(&self.steps);
    Box::pin(async move {
      let handle = steps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&step_id);
      if let Some(handle) = handle {
        handle.end(error).await;
      }
    })
  }

  fn soft_error(&self, message: String) -> BridgeFuture<()> {
    Self::lock(&self.soft_errors).push(message.clone());
    let info = Arc::clone(&self.test_info);
    Box::pin(async move {
      info
        .add_soft_error(ferridriver_test::model::TestFailure {
          message,
          stack: None,
          diff: None,
          screenshot: None,
        })
        .await;
    })
  }

  fn set_skip(&self, reason: Option<String>) {
    self.modifiers.skipped.store(true, Ordering::Relaxed);
    *Self::lock(&self.modifiers.skip_reason) = reason;
  }

  fn set_expected_failure(&self) {
    self.modifiers.expected_failure.store(true, Ordering::Relaxed);
  }

  fn set_slow(&self) {
    self.modifiers.slow.store(true, Ordering::Relaxed);
    // Playwright triples the budget; keep the VM interrupt deadline in
    // step with the worker's extended timeout.
    self.session.session().arm_deadline(self.base_timeout * 3);
  }

  fn set_timeout_override(&self, ms: u64) {
    *Self::lock(&self.modifiers.timeout_override) = Some(ms);
    self.session.session().arm_deadline(Duration::from_millis(ms));
  }

  fn output_path(&self, parts: &[String]) -> String {
    let mut p = self.test_info.output_dir.clone();
    for part in parts {
      p.push(part);
    }
    let _ = std::fs::create_dir_all(&self.test_info.output_dir);
    p.display().to_string()
  }

  fn snapshot_path(&self, name: &str) -> String {
    self.test_info.snapshot_dir.join(name).display().to_string()
  }

  fn errors(&self) -> Vec<String> {
    Self::lock(&self.soft_errors).clone()
  }
}

/// Annotations lowered for the `testInfo.annotations` getter from the
/// plan's core annotations.
pub fn static_annotation_pairs(annotations: &[TestAnnotation]) -> Vec<(String, Option<String>)> {
  annotations
    .iter()
    .filter_map(|a| match a {
      TestAnnotation::Info { type_name, description } => {
        Some((type_name.clone(), Some(description.clone()).filter(|d| !d.is_empty())))
      },
      TestAnnotation::Tag(_) | TestAnnotation::Only => None,
      TestAnnotation::Skip { reason, .. } => Some(("skip".to_string(), reason.clone())),
      TestAnnotation::Fixme { reason, .. } => Some(("fixme".to_string(), reason.clone())),
      TestAnnotation::Fail { reason, .. } => Some(("fail".to_string(), reason.clone())),
      TestAnnotation::Slow { reason, .. } => Some(("slow".to_string(), reason.clone())),
    })
    .collect()
}
