//! [`TestHostBridge`] implementation over the core `TestInfo` /
//! `TestModifiers` — the seam a running JS test reaches the runner
//! through (`testInfo.*`, `test.step`, runtime modifiers).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferridriver_script::{BridgeFuture, CompiledBundle, SnapshotTarget, TestHostBridge};
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
  /// Counter behind Playwright's auto-generated snapshot names
  /// (`{title}-{n}`).
  snapshot_counter: AtomicUsize,
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
      snapshot_counter: AtomicUsize::new(0),
    }
  }

  /// `toMatchSnapshot()` / `toHaveScreenshot()` without a name — the
  /// Playwright convention: sanitized test title + running counter.
  fn auto_snapshot_name(&self) -> String {
    let n = self.snapshot_counter.fetch_add(1, Ordering::Relaxed) + 1;
    let title: String = self
      .test_info
      .test_id
      .name
      .chars()
      .map(|c| {
        if c.is_alphanumeric() || c == '-' || c == '_' {
          c
        } else {
          '-'
        }
      })
      .collect();
    format!("{title}-{n}")
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
    let abs = crate::translate::resolve_source(&self.cwd, &src);
    let file = abs
      .strip_prefix(self.cwd.as_path())
      .map_or_else(|_| abs.display().to_string(), |r| r.display().to_string());
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

  fn record_soft_error(&self, message: String, diff: Option<String>) {
    // One store: the test's own collector, which decides the outcome and
    // backs `testInfo.errors`. Recording is synchronous because a value
    // matcher has no `await` to spend.
    self.test_info.add_soft_error(ferridriver_test::model::TestFailure {
      message,
      stack: None,
      diff,
      screenshot: None,
    });
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
    self.test_info.soft_error_messages()
  }

  fn match_text_snapshot(&self, target: SnapshotTarget, name: Option<String>) -> BridgeFuture<Result<(), String>> {
    let info = Arc::clone(&self.test_info);
    let name = name.unwrap_or_else(|| self.auto_snapshot_name());
    Box::pin(async move {
      let actual = match target {
        SnapshotTarget::Value(s) => s,
        SnapshotTarget::Locator(locator) => locator
          .text_content()
          .await
          .map_err(|e| format!("toMatchSnapshot: reading text content: {e}"))?
          .unwrap_or_default(),
        SnapshotTarget::Page(_) => {
          return Err(
            "toMatchSnapshot applies to a string value or a locator — use toHaveScreenshot for pages".to_string(),
          );
        },
      };
      ferridriver_test::snapshot::assert_snapshot(&info, &actual, &name, false).map_err(|f| f.message)
    })
  }

  fn match_screenshot(
    &self,
    target: SnapshotTarget,
    name: Option<String>,
    options: serde_json::Value,
  ) -> BridgeFuture<Result<(), String>> {
    let info = Arc::clone(&self.test_info);
    let name = name.unwrap_or_else(|| self.auto_snapshot_name());
    Box::pin(async move {
      let opts = screenshot_options_from_json(&options, info.ignore_snapshots);
      let png = match target {
        SnapshotTarget::Locator(locator) => ferridriver_test::expect::locator::capture_with_options(&locator, &opts)
          .await
          .map_err(|f| f.message)?,
        SnapshotTarget::Page(page) => page
          .screenshot()
          .await
          .map_err(|e| format!("toHaveScreenshot: page screenshot failed: {e}"))?,
        SnapshotTarget::Value(_) => {
          return Err("toHaveScreenshot applies to a locator or a page".to_string());
        },
      };
      let update = matches!(
        info.update_snapshots,
        ferridriver_test::config::UpdateSnapshotsMode::All | ferridriver_test::config::UpdateSnapshotsMode::Changed
      );
      ferridriver_test::snapshot::compare_screenshot_png_in(&info.snapshot_dir, &png, &name, &opts, update)
        .map_err(|f| f.message)
    })
  }

  fn match_aria_snapshot(
    &self,
    target: SnapshotTarget,
    expected_yaml: String,
    is_not: bool,
    timeout_ms: Option<u64>,
  ) -> BridgeFuture<Result<(), String>> {
    let timeout = Duration::from_millis(
      timeout_ms
        .unwrap_or_else(|| u64::try_from(ferridriver_expect::default_expect_timeout().as_millis()).unwrap_or(5000)),
    );
    Box::pin(async move {
      match target {
        SnapshotTarget::Locator(locator) => {
          let mut e = ferridriver_expect::expect(&locator).with_timeout(timeout);
          if is_not {
            e = e.not();
          }
          ferridriver_test::expect::LocatorSnapshotMatchers::to_match_aria_snapshot(&e, &expected_yaml)
            .await
            .map_err(|f| f.message)
        },
        SnapshotTarget::Page(page) => {
          let mut e = ferridriver_expect::expect(&page).with_timeout(timeout);
          if is_not {
            e = e.not();
          }
          ferridriver_test::expect::PageSnapshotMatchers::to_match_aria_snapshot(&e, &expected_yaml)
            .await
            .map_err(|f| f.message)
        },
        SnapshotTarget::Value(_) => Err("toMatchAriaSnapshot applies to a locator or a page".to_string()),
      }
    })
  }
}

/// Lower the raw Playwright option bag into the runner's
/// [`ferridriver_test::expect::ScreenshotMatcherOptions`].
fn screenshot_options_from_json(
  v: &serde_json::Value,
  ignore_snapshots: bool,
) -> ferridriver_test::expect::ScreenshotMatcherOptions {
  let f = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
  let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
  ferridriver_test::expect::ScreenshotMatcherOptions {
    threshold: f("threshold"),
    max_diff_pixels: v.get("maxDiffPixels").and_then(serde_json::Value::as_u64),
    max_diff_pixel_ratio: f("maxDiffPixelRatio"),
    mask_color: s("maskColor"),
    animations: s("animations"),
    caret: s("caret"),
    scale: s("scale"),
    style_path: s("stylePath").map(std::path::PathBuf::from),
    clip: v.get("clip").map(|c| ferridriver_test::expect::ScreenshotClip {
      x: c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      y: c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      width: c.get("width").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      height: c.get("height").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
    }),
    mask: v
      .get("mask")
      .and_then(serde_json::Value::as_array)
      .map(|a| a.iter().filter_map(|m| m.as_str().map(str::to_string)).collect())
      .unwrap_or_default(),
    ignore: ignore_snapshots,
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
