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
  failure_context: Mutex<Option<String>>,
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
      failure_context: Mutex::new(None),
    }
  }

  fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// What [`TestHostBridge::set_failure_context`] recorded, for the
  /// [`crate::model::TestFailure`] the host builds from the thrown error.
  pub fn take_failure_context(&self) -> Option<String> {
    Self::lock(&self.failure_context).take()
  }

  /// Flush runtime annotations into the core `TestInfo` so reporters
  /// see them. Called by the test closure after the body settles (the
  /// bridge's own methods are sync; `TestInfo.annotations` is async).
  pub async fn flush(&self) {
    let drained: Vec<(String, Option<String>)> = Self::lock(&self.annotations).drain(..).collect();
    for (type_name, description) in drained {
      self.test_info.annotate_runtime(type_name, description).await;
    }
    // Close any step left open by a mid-step failure so reporters and
    // the trace never see a dangling span.
    self.test_info.close_open_steps("step never completed (test aborted)");
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

impl InfoBridge {
  /// Resolve the host's own coordinates back to authored source, and an
  /// explicit location to the path reporters show.
  fn resolve_spec(&self, mut spec: crate::step::StepSpec) -> crate::step::StepSpec {
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
    spec
  }
}

/// The host's step driver: resolve the host's own coordinates back to
/// authored source, then let the runner apply every rule.
impl crate::step::StepDriver for InfoBridge {
  fn begin_step(&self, spec: crate::step::StepSpec) -> crate::step::StepFuture<'_, crate::step::StepStarted> {
    crate::step::StepDriver::begin_step(&*self.test_info, self.resolve_spec(spec))
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

  fn record_soft_error(&self, message: String, diff: Option<String>, error_context: Option<String>) {
    // One store: the test's own collector, which decides the outcome and
    // backs `testInfo.errors`. Recording is synchronous because a value
    // matcher has no `await` to spend.
    self.test_info.add_soft_error(crate::model::TestFailure {
      message,
      stack: None,
      diff,
      screenshot: None,
      error_context,
    });
  }

  fn set_failure_context(&self, error_context: Option<String>) {
    *Self::lock(&self.failure_context) = error_context;
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
    self.modifiers.timeout_updates.send_replace(self.base_timeout * 3);
  }

  fn set_timeout_override(&self, ms: u64) {
    *Self::lock(&self.modifiers.timeout_override) = Some(ms);
    let timeout = Duration::from_millis(ms);
    if timeout.is_zero() {
      self.deadline.disarm();
    } else {
      self.deadline.arm(timeout);
    }
    self.modifiers.timeout_updates.send_replace(timeout);
  }

  fn output_path(&self, parts: &[String]) -> String {
    let mut p = self.test_info.output_dir.clone();
    for part in parts {
      p.push(part);
    }
    let _ = std::fs::create_dir_all(&self.test_info.output_dir);
    p.display().to_string()
  }

  fn snapshot_path(&self, name: &[String], kind: &str) -> Result<String, String> {
    let kind = crate::snapshot_path::SnapshotKind::parse(kind).ok_or_else(|| {
      format!(r#"testInfo.snapshotPath: unknown kind "{kind}", must be one of "snapshot", "screenshot" or "aria""#)
    })?;
    // The same resolver a matcher uses, and deliberately WITHOUT
    // consuming an index: asking where a snapshot lives must not change
    // where the next one goes.
    let resolved =
      self
        .test_info
        .resolve_snapshot_paths(kind, &crate::snapshot_path::SnapshotName::from_parts(name), false);
    Ok(resolved.absolute_snapshot_path.display().to_string())
  }

  fn errors(&self) -> Vec<String> {
    self.test_info.soft_error_messages()
  }

  fn call_owner(&self) -> Option<String> {
    Some(self.test_info.call_owner())
  }

  fn open_step_now(&self, spec: crate::step::StepSpec, nests: bool) -> Option<String> {
    Some(self.test_info.open_step_now(self.resolve_spec(spec), nests).step_id)
  }

  fn close_step_now(&self, step_id: &str, outcome: crate::step::StepOutcome) {
    self.test_info.close_step_now(step_id, outcome);
  }

  fn match_value_snapshot(
    &self,
    received: crate::snapshot::SnapshotValue,
    name: crate::snapshot_path::SnapshotName,
    options: serde_json::Value,
    is_not: bool,
  ) -> Result<(), String> {
    let info = &self.test_info;
    let defaults = &info.expect.to_match_snapshot;
    let parsed = screenshot_options_from_json(&options, false)?;
    let comparator = crate::comparator::ImageComparatorOptions {
      threshold: parsed.threshold.or(defaults.threshold),
      max_diff_pixels: parsed.max_diff_pixels.or(defaults.max_diff_pixels.map(u64::from)),
      max_diff_pixel_ratio: parsed.max_diff_pixel_ratio.or(defaults.max_diff_pixel_ratio),
      comparator: parsed.comparator,
    };
    let soft =
      crate::snapshot::match_value_snapshot(info, &received, &name, &comparator, is_not).map_err(|f| f.message)?;
    record_missing_baseline(info, soft);
    Ok(())
  }

  fn match_screenshot(
    &self,
    target: SnapshotTarget,
    name: crate::snapshot_path::SnapshotName,
    options: serde_json::Value,
    is_not: bool,
  ) -> BridgeFuture<Result<(), String>> {
    let info = Arc::clone(&self.test_info);
    Box::pin(async move {
      // Playwright layers the call's bag over `expect.toHaveScreenshot`
      // (`matchers/toMatchSnapshot.ts:121-127`), so a suite that sets a
      // threshold once in config gets it on every call that did not
      // name its own.
      let opts = screenshot_options_from_json(&options, info.ignore_snapshots)?
        .with_config_defaults(&info.expect.to_have_screenshot);
      // `timeout` on the call, else `expect.toHaveScreenshot.timeout`,
      // else the expect default — the window the capture retries in.
      let timeout = opts
        .timeout
        .unwrap_or_else(|| Duration::from_millis(info.expect.timeout_ms()));
      let resolved = info.resolve_snapshot_paths(crate::snapshot_path::SnapshotKind::Screenshot, &name, true);
      let paths = crate::snapshot::ScreenshotFiles::new(
        resolved.absolute_snapshot_path,
        &info.output_dir.join(&resolved.relative_output_path),
      );
      let soft = crate::snapshot::to_have_screenshot(&paths, &opts, info.update_snapshots, is_not, timeout, || {
        let target = target.clone();
        let opts = &opts;
        async move {
          match target {
            SnapshotTarget::Locator(locator) => crate::expect::locator::capture_with_options(&locator, opts).await,
            SnapshotTarget::Page(page) => crate::expect::locator::capture_page_with_options(&page, opts).await,
            SnapshotTarget::Value(_) => Err(crate::model::TestFailure {
              message: "toHaveScreenshot applies to a locator or a page".to_string(),
              stack: None,
              diff: None,
              screenshot: None,
              error_context: None,
            }),
          }
        }
      })
      .await
      .map_err(|f| f.message)?;
      record_missing_baseline(&info, soft);
      Ok(())
    })
  }

  fn match_aria_snapshot(
    &self,
    target: SnapshotTarget,
    expected: super::AriaExpectation,
    is_not: bool,
    timeout_ms: Option<u64>,
  ) -> BridgeFuture<Result<(), String>> {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or_else(|| self.test_info.expect.timeout_ms()));
    let info = Arc::clone(&self.test_info);
    Box::pin(async move {
      let locator = match &target {
        SnapshotTarget::Locator(locator) => locator.clone(),
        // Page-scoped `to.match.aria` matches against `document.body`.
        SnapshotTarget::Page(page) => page.locator("body"),
        SnapshotTarget::Value(_) => return Err("toMatchAriaSnapshot applies to a locator or a page".to_string()),
      };
      let expected_yaml = match expected {
        super::AriaExpectation::File(name) => {
          let soft = crate::snapshot::match_aria_file(&info, &name, is_not, timeout, |template| {
            let locator = locator.clone();
            async move { locator.match_aria_snapshot(&template).await }
          })
          .await
          .map_err(|f| f.message)?;
          record_missing_baseline(&info, soft);
          return Ok(());
        },
        super::AriaExpectation::Template(yaml) => {
          crate::expect::aria_template_with_children(&yaml, info.expect.to_match_aria_snapshot.children.as_deref())
        },
      };
      if let SnapshotTarget::Page(page) = target {
        let mut e = ferridriver_expect::expect(&page).with_timeout(timeout);
        if is_not {
          e = e.not();
        }
        crate::expect::PageSnapshotMatchers::to_match_aria_snapshot(&e, &expected_yaml)
          .await
          .map_err(|f| f.message)
      } else {
        let mut e = ferridriver_expect::expect(&locator).with_timeout(timeout);
        if is_not {
          e = e.not();
        }
        crate::expect::LocatorSnapshotMatchers::to_match_aria_snapshot(&e, &expected_yaml)
          .await
          .map_err(|f| f.message)
      }
    })
  }
}

/// Lower the raw Playwright option bag into the runner's
/// [`crate::expect::ScreenshotMatcherOptions`].
fn screenshot_options_from_json(
  v: &serde_json::Value,
  ignore_snapshots: bool,
) -> Result<crate::expect::ScreenshotMatcherOptions, String> {
  let f = |k: &str| v.get(k).and_then(serde_json::Value::as_f64);
  let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
  // `SnapshotHelper`'s checks (`matchers/toMatchSnapshot.ts`).
  if f("maxDiffPixels").is_some_and(|n| n < 0.0) {
    return Err("`maxDiffPixels` option value must be non-negative integer".to_string());
  }
  if f("maxDiffPixelRatio").is_some_and(|r| !(0.0..=1.0).contains(&r)) {
    return Err("`maxDiffPixelRatio` option value must be between 0 and 1".to_string());
  }
  let comparator = s("_comparator")
    .map(|name| name.parse())
    .transpose()?
    .unwrap_or_default();
  Ok(crate::expect::ScreenshotMatcherOptions {
    threshold: f("threshold"),
    max_diff_pixels: v.get("maxDiffPixels").and_then(serde_json::Value::as_u64),
    max_diff_pixel_ratio: f("maxDiffPixelRatio"),
    comparator,
    mask_color: s("maskColor"),
    animations: s("animations"),
    caret: s("caret"),
    scale: s("scale"),
    style_path: match v.get("stylePath") {
      Some(serde_json::Value::String(one)) => vec![std::path::PathBuf::from(one)],
      Some(serde_json::Value::Array(many)) => many
        .iter()
        .filter_map(|e| e.as_str().map(std::path::PathBuf::from))
        .collect(),
      _ => Vec::new(),
    },
    timeout: v
      .get("timeout")
      .and_then(serde_json::Value::as_u64)
      .map(Duration::from_millis),
    clip: v.get("clip").map(|c| crate::expect::ScreenshotClip {
      x: c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      y: c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      width: c.get("width").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
      height: c.get("height").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
    }),
    // Playwright takes `Array<Locator>`; a Locator lowers to its
    // selector on the way through, and a bare selector string is
    // accepted too because that is what a TOML/JSON caller can write.
    mask: v
      .get("mask")
      .and_then(serde_json::Value::as_array)
      .map(|a| {
        a.iter()
          .filter_map(|m| match m {
            serde_json::Value::String(sel) => Some(sel.clone()),
            other => other
              .get("selector")
              .and_then(serde_json::Value::as_str)
              .map(str::to_string),
          })
          .collect()
      })
      .unwrap_or_default(),
    full_page: v.get("fullPage").and_then(serde_json::Value::as_bool),
    omit_background: v.get("omitBackground").and_then(serde_json::Value::as_bool),
    ignore: ignore_snapshots,
  })
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
    expect: Arc::clone(&info.expect),
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
      project_name: info.project_name(),
      // The worker's own config, lowered by the SAME function the
      // reporter API uses, so `testInfo.config` and what a reporter is
      // handed cannot describe different runs.
      config: info
        .config_snapshot
        .as_ref()
        .map_or(serde_json::Value::Null, |cfg| crate::reporter::api::full_config(cfg)),
    },
  }
}

/// A baseline written under `updateSnapshots: 'default'` fails the test
/// once it ends, and no retry can fix that.
fn record_missing_baseline(info: &TestInfo, soft: Option<String>) {
  if let Some(message) = soft {
    info.add_soft_error(crate::model::TestFailure {
      message,
      stack: None,
      diff: None,
      screenshot: None,
      error_context: None,
    });
    info.forbid_retry();
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
      TestAnnotation::Tag(_) | TestAnnotation::Only | TestAnnotation::Lock(_) => None,
      TestAnnotation::Skip { reason, .. } => Some(("skip".to_string(), reason.clone())),
      TestAnnotation::Fixme { reason, .. } => Some(("fixme".to_string(), reason.clone())),
      TestAnnotation::Fail { reason, .. } => Some(("fail".to_string(), reason.clone())),
      TestAnnotation::Slow { reason, .. } => Some(("slow".to_string(), reason.clone())),
    })
    .collect()
}
