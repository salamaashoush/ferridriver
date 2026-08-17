//! [`TestHostBridge`] implementation over the core `TestInfo` /
//! `TestModifiers` — the seam a running JS test reaches the runner
//! through (`testInfo.*`, `test.step`, runtime modifiers).
//!
//! One implementation, both JS hosts: a `ferridriver test` spec and a
//! `ferridriver bdd` scenario reach the runner through the same object,
//! so `testInfo.attach`, `test.step`, soft assertions and the snapshot
//! matchers cannot behave differently depending on which one is running.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{BridgeFuture, DeadlineControl, SnapshotTarget, SourceMap, TestHostBridge, TestInfoData, TestWorldData};
use crate::config::BrowserConfig;
use crate::model::{AttachmentBody, ExpectedStatus, StepLocation, TestAnnotation, TestInfo, TestModifiers};

pub struct InfoBridge {
  test_info: Arc<TestInfo>,
  modifiers: Arc<TestModifiers>,
  /// The host's interrupt deadline: `test.slow()` and
  /// `testInfo.setTimeout()` re-arm it so a runaway body is still
  /// force-halted at the budget the test just asked for.
  deadline: Arc<dyn DeadlineControl>,
  /// The host's map back to authored source, for step locations.
  source_map: Arc<dyn SourceMap>,
  cwd: Arc<PathBuf>,
  /// Base per-test timeout — `test.slow()` re-arms the VM deadline to
  /// three times this (the worker applies the same multiplier to its
  /// own budget).
  base_timeout: Duration,
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
  /// The host's `testInfo.titlePath`, which a step's own title path
  /// continues. Empty means "the test's own", which is what a host with
  /// no richer path of its own wants.
  title_path: Vec<String>,
}

impl InfoBridge {
  pub fn new(
    test_info: Arc<TestInfo>,
    modifiers: Arc<TestModifiers>,
    deadline: Arc<dyn DeadlineControl>,
    source_map: Arc<dyn SourceMap>,
    cwd: Arc<PathBuf>,
    base_timeout: Duration,
    static_annotations: Vec<(String, Option<String>)>,
  ) -> Self {
    Self {
      test_info,
      modifiers,
      deadline,
      source_map,
      cwd,
      base_timeout,
      annotations: Mutex::new(Vec::new()),
      static_annotations,
      attachment_count: AtomicUsize::new(0),
      snapshot_counter: AtomicUsize::new(0),
      title_path: Vec::new(),
    }
  }

  /// The title path the host's `testInfo.titlePath` shows, so
  /// `stepInfo.titlePath` continues the same one.
  #[must_use]
  pub fn with_title_path(mut self, title_path: Vec<String>) -> Self {
    self.title_path = title_path;
    self
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
    self
      .test_info
      .close_open_steps("step never completed (test aborted)")
      .await;
  }

  fn remap_location(&self, location: Option<(u32, u32)>) -> Option<StepLocation> {
    let (line, col) = location?;
    let (src, src_line, src_col) = self.source_map.remap(line, col)?;
    Some(StepLocation {
      file: self.relative(&src),
      line: src_line,
      column: src_col,
    })
  }

  /// Reporters show a path relative to where the run was started, so an
  /// explicit `{ location }` is normalized the same way a captured
  /// frame is — otherwise one step names an absolute path and the next
  /// a relative one.
  fn relative(&self, file: &str) -> String {
    let path = std::path::Path::new(file);
    path
      .strip_prefix(self.cwd.as_path())
      .map_or_else(|_| path.display().to_string(), |r| r.display().to_string())
  }
}

/// The host's step driver: resolve the host's own coordinates back to
/// authored source, then let the runner apply every rule.
impl crate::step::StepDriver for InfoBridge {
  fn begin_step(&self, mut spec: crate::step::StepSpec) -> crate::step::StepFuture<'_, crate::step::StepStarted> {
    spec.frames = spec
      .frames
      .into_iter()
      .filter_map(|frame| match frame {
        crate::step::StepFrame::Host { line, column } => self
          .remap_location(Some((line, column)))
          .map(crate::step::StepFrame::Source),
        source => Some(source),
      })
      .collect();
    if let Some(location) = spec.options.location.as_mut() {
      location.file = self.relative(&location.file);
    }
    self.test_info.begin_step_spec(
      spec,
      (!self.title_path.is_empty()).then_some(self.title_path.as_slice()),
    )
  }

  fn end_step(&self, step_id: String, outcome: crate::step::StepOutcome) -> crate::step::StepFuture<'_, ()> {
    crate::step::StepDriver::end_step(&*self.test_info, step_id, outcome)
  }
}

impl TestHostBridge for InfoBridge {
  fn attach(&self, name: String, content_type: String, body: Vec<u8>, step_id: Option<String>) -> BridgeFuture<()> {
    let info = Arc::clone(&self.test_info);
    self.attachment_count.fetch_add(1, Ordering::Relaxed);
    Box::pin(async move {
      info
        .attach_to_step(name, content_type, AttachmentBody::Bytes(body), step_id)
        .await;
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

  fn record_soft_error(&self, message: String, diff: Option<String>) {
    // One store: the test's own collector, which decides the outcome and
    // backs `testInfo.errors`. Recording is synchronous because a value
    // matcher has no `await` to spend.
    self.test_info.add_soft_error(crate::model::TestFailure {
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
    self.deadline.arm(self.base_timeout * 3);
  }

  fn set_timeout_override(&self, ms: u64) {
    *Self::lock(&self.modifiers.timeout_override) = Some(ms);
    self.deadline.arm(Duration::from_millis(ms));
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
      crate::snapshot::assert_snapshot(&info, &actual, &name, false).map_err(|f| f.message)
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
        SnapshotTarget::Locator(locator) => crate::expect::locator::capture_with_options(&locator, &opts)
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
        crate::config::UpdateSnapshotsMode::All | crate::config::UpdateSnapshotsMode::Changed
      );
      crate::snapshot::compare_screenshot_png_in(&info.snapshot_dir, &png, &name, &opts, update).map_err(|f| f.message)
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
          crate::expect::LocatorSnapshotMatchers::to_match_aria_snapshot(&e, &expected_yaml)
            .await
            .map_err(|f| f.message)
        },
        SnapshotTarget::Page(page) => {
          let mut e = ferridriver_expect::expect(&page).with_timeout(timeout);
          if is_not {
            e = e.not();
          }
          crate::expect::PageSnapshotMatchers::to_match_aria_snapshot(&e, &expected_yaml)
            .await
            .map_err(|f| f.message)
        },
        SnapshotTarget::Value(_) => Err("toMatchAriaSnapshot applies to a locator or a page".to_string()),
      }
    })
  }
}

/// Lower the raw Playwright option bag into the runner's
/// [`crate::expect::ScreenshotMatcherOptions`].
fn screenshot_options_from_json(
  v: &serde_json::Value,
  ignore_snapshots: bool,
) -> crate::expect::ScreenshotMatcherOptions {
  let f = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
  let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
  crate::expect::ScreenshotMatcherOptions {
    threshold: f("threshold"),
    max_diff_pixels: v.get("maxDiffPixels").and_then(serde_json::Value::as_u64),
    max_diff_pixel_ratio: f("maxDiffPixelRatio"),
    mask_color: s("maskColor"),
    animations: s("animations"),
    caret: s("caret"),
    scale: s("scale"),
    style_path: s("stylePath").map(std::path::PathBuf::from),
    clip: v.get("clip").map(|c| crate::expect::ScreenshotClip {
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

/// Overlay one `use` bag onto another, key by key — Playwright's rule
/// for `use` precedence, which is a shallow take-the-inner-value, never
/// a deep merge. A non-object base starts from `{}`.
#[must_use]
pub fn merge_use_options(base: Option<&serde_json::Value>, overlay: Option<&serde_json::Value>) -> serde_json::Value {
  let mut out = match base {
    Some(serde_json::Value::Object(map)) => map.clone(),
    _ => serde_json::Map::new(),
  };
  if let Some(serde_json::Value::Object(inc)) = overlay {
    for (k, v) in inc {
      out.insert(k.clone(), v.clone());
    }
  }
  serde_json::Value::Object(out)
}

/// Everything a JS host knows about one test or scenario that is not a
/// live browser handle. [`world_data`] lowers it into the
/// [`TestWorldData`] the VM builds its fixtures object from; the host
/// fills in `page` / `context` / `request` / `browser` afterwards from
/// whatever pool it resolved them out of.
pub struct WorldMeta<'a> {
  pub test_info: &'a TestInfo,
  pub title: &'a str,
  pub title_path: &'a [String],
  pub file: &'a str,
  pub line: u32,
  pub tags: &'a [String],
  pub expected_status: ExpectedStatus,
  /// Fallback browser config for a run with no per-project snapshot.
  /// The worker's own `TestInfo.config_snapshot` wins when present: a
  /// multi-project run gives each test its project's browser, and the
  /// translate-time config would report the root one.
  pub browser_config: &'a BrowserConfig,
  /// Config-level `baseURL`, below the `use` bag and above the
  /// environment.
  pub base_url: Option<&'a str>,
  /// Effective `use` bag (config ⊕ file ⊕ suite ⊕ test, or ⊕ the
  /// scenario's `@use(...)` tags). Option fixtures read their overrides
  /// from it, and three Playwright options are lifted out of it here.
  pub use_options: serde_json::Value,
}

/// Lower one test's metadata for the VM. The single place `TestInfo`
/// becomes `TestInfoData`, so the spec host and the BDD host cannot
/// disagree about what `testInfo` says.
#[must_use]
pub fn world_data(meta: WorldMeta<'_>) -> TestWorldData {
  let info = meta.test_info;
  let browser = info
    .config_snapshot
    .as_ref()
    .map_or(meta.browser_config, |cfg| &cfg.browser);
  let bag = |key: &str| meta.use_options.get(key);
  TestWorldData {
    page: None,
    context: None,
    request: None,
    browser: None,
    browser_name: browser.browser.clone(),
    headless: browser.headless,
    is_mobile: bag("isMobile").and_then(serde_json::Value::as_bool).unwrap_or(false),
    has_touch: bag("hasTouch").and_then(serde_json::Value::as_bool).unwrap_or(false),
    base_url: bag("baseURL")
      .and_then(serde_json::Value::as_str)
      .map(String::from)
      .or_else(|| meta.base_url.map(String::from))
      .or_else(crate::config::base_url_from_env),
    use_options: meta.use_options,
    info: TestInfoData {
      title: meta.title.to_string(),
      title_path: meta.title_path.to_vec(),
      file: meta.file.to_string(),
      line: meta.line,
      column: info.column.unwrap_or(0),
      retry: info.retry,
      worker_index: info.worker_index,
      parallel_index: info.parallel_index,
      repeat_each_index: info.repeat_each_index,
      timeout_ms: u64::try_from(info.timeout.as_millis()).unwrap_or(u64::MAX),
      expected_status: match meta.expected_status {
        ExpectedStatus::Pass => "passed".to_string(),
        ExpectedStatus::Fail => "failed".to_string(),
      },
      tags: meta.tags.to_vec(),
      output_dir: info.output_dir.display().to_string(),
      snapshot_dir: info.snapshot_dir.display().to_string(),
      // The runner owns the suffix (`TestInfo.snapshot_suffix` is async
      // state the VM never reads through here); the snapshot matchers
      // go through the bridge, which reads the live value.
      snapshot_suffix: String::new(),
      project_name: info.project.as_ref().map(|p| p.name.clone()),
    },
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
