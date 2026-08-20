//! Snapshot / screenshot / aria matchers for `Expect<Locator>`. These
//! stay in the test runner because they need `TestInfo`-keyed snapshot
//! directories, the `image` crate, and the Playwright-bundled aria
//! renderer's YAML output format. Every other locator matcher lives in
//! [`ferridriver_expect`] (single source of truth).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use ferridriver::Locator;
use ferridriver_expect::{Expect, ExpectContext, MatchError, poll_traced};

use super::ScreenshotMatcherOptions;
use crate::model::TestFailure;

fn locator_ctx(locator: &Locator, method: &'static str, is_not: bool, is_soft: bool) -> ExpectContext {
  ExpectContext {
    method,
    subject: format!("locator('{}')", locator.selector()),
    is_not,
    is_soft,
  }
}

async fn poll_until_test<F, Fut>(
  locator: &Locator,
  timeout: Duration,
  ctx: ExpectContext,
  check: F,
) -> Result<(), TestFailure>
where
  F: FnMut() -> Fut,
  Fut: Future<Output = Result<(), MatchError>>,
{
  let params = serde_json::json!({
    "selector": locator.selector(),
    "isNot": ctx.is_not,
    "timeout": u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
  });
  poll_traced(Some(&**locator.page()), params, timeout, ctx, check)
    .await
    .map_err(Into::into)
}

/// Snapshot matchers for `expect(locator)`. Imported via
/// `use ferridriver_test::expect::LocatorSnapshotMatchers;` at the call
/// site so the methods light up alongside the shared web-first
/// matchers from [`ferridriver_expect`].
#[allow(async_fn_in_trait)]
pub trait LocatorSnapshotMatchers {
  /// Compare the element's text content against a stored `.snap` file.
  async fn to_match_snapshot(&self, name: &str) -> Result<(), TestFailure>;

  /// Compare the element's screenshot to a baseline PNG (default
  /// options).
  async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure>;

  /// Playwright `toHaveScreenshot(name, options?)` — full capture
  /// option bag.
  async fn to_have_screenshot_with(&self, name: &str, options: ScreenshotMatcherOptions) -> Result<(), TestFailure>;

  /// Playwright `toMatchAriaSnapshot(yaml)` — compares the live ARIA
  /// tree against the Playwright-style YAML template.
  async fn to_match_aria_snapshot(&self, expected_yaml: &str) -> Result<(), TestFailure>;
}

impl LocatorSnapshotMatchers for Expect<'_, Locator> {
  async fn to_match_snapshot(&self, name: &str) -> Result<(), TestFailure> {
    let locator = self.subject;
    let actual = locator.text_content().await.unwrap_or(None).unwrap_or_default();
    let snap_dir = std::path::PathBuf::from("__snapshots__");
    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
    let info = crate::model::TestInfo {
      test_id: crate::model::TestId {
        file: String::new(),
        suite: None,
        name: name.to_string(),
        line: None,
        column: None,
      },
      title_path: vec![name.to_string()],
      retry: 0,
      worker_index: 0,
      parallel_index: 0,
      repeat_each_index: 0,
      output_dir: std::path::PathBuf::from("test-results"),
      snapshot_dir: snap_dir,
      snapshot_path_template: None,
      update_snapshots: crate::config::UpdateSnapshotsMode::default(),
      ignore_snapshots: false,
      attachments: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      steps: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      soft_errors: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
      errors: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      snapshot_suffix: std::sync::Arc::new(tokio::sync::Mutex::new(String::new())),
      column: None,
      project: None,
      config_snapshot: None,
      expect: std::sync::Arc::new(crate::config::ExpectConfig::default()),
      config_dir: Default::default(),
      test_dir: Default::default(),
      snapshot_names: Default::default(),
      aria_snapshot_names: Default::default(),
      timeout: self.timeout,
      tags: Vec::new(),
      start_time: std::time::Instant::now(),
      event_bus: None,
      annotations: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      trace_composite: std::sync::Arc::new(std::sync::Mutex::new(None)),
      trace_step_calls: std::sync::Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      open_steps: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
      output: std::sync::Arc::new(std::sync::Mutex::new(crate::model::TestOutput::default())),
    };
    crate::snapshot::assert_snapshot(
      &info,
      &actual,
      &crate::snapshot_path::SnapshotName::One(name.to_string()),
      update,
    )
  }

  async fn to_have_screenshot(&self, name: &str) -> Result<(), TestFailure> {
    self
      .to_have_screenshot_with(name, ScreenshotMatcherOptions::default())
      .await
  }

  async fn to_have_screenshot_with(&self, name: &str, options: ScreenshotMatcherOptions) -> Result<(), TestFailure> {
    let locator = self.subject;
    // What the call did not set comes from `expect.toHaveScreenshot`.
    let options = options.with_config_defaults(&crate::expect::current_expect_config().to_have_screenshot);
    let timeout = options.timeout.unwrap_or(self.timeout);
    let snap_dir = std::env::var("SNAPSHOT_DIR")
      .map(std::path::PathBuf::from)
      .unwrap_or_else(|_| std::path::PathBuf::from("__snapshots__"));
    let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
    let paths = crate::snapshot::ScreenshotFiles::beside(&snap_dir, name);
    crate::snapshot::screenshot_until_match(&paths, &options, update, timeout, || {
      capture_with_options(locator, &options)
    })
    .await
  }

  async fn to_match_aria_snapshot(&self, expected_yaml: &str) -> Result<(), TestFailure> {
    let locator = self.subject;
    let is_not = self.is_not;
    // `expect.toMatchAriaSnapshot.children` applies to every template
    // that does not declare its own `/children`.
    let cfg = crate::expect::current_expect_config();
    let expected_yaml =
      crate::expect::aria_template_with_children(expected_yaml, cfg.to_match_aria_snapshot.children.as_deref());
    let expected_yaml = expected_yaml.as_str();
    poll_until_test(
      locator,
      self.timeout,
      locator_ctx(locator, "toMatchAriaSnapshot", is_not, self.is_soft),
      || {
        let expected_yaml = expected_yaml.to_string();
        async move {
          // Playwright's own matcher, over the parsed template — the
          // expected tree is a SUBSET, so extra siblings/attributes and
          // intervening depth are fine. A hand-rolled line-subsequence
          // comparison rejected every partial template.
          let result = locator
            .match_aria_snapshot(&expected_yaml)
            .await
            .map_err(|e| MatchError::new(expected_yaml.clone(), e.to_string()))?;
          if result.matches == is_not {
            Err(MatchError::new(
              format!("{}\n{expected_yaml}", if is_not { "not matching" } else { "matching" }),
              result.received,
            ))
          } else {
            Ok(())
          }
        }
      },
    )
    .await
  }
}

// ── Screenshot capture wrapper (§7.17 capture-time options) ─────────────────

/// Capture a locator screenshot honoring the matcher's capture options
/// (animations, caret, masks, clip, style). Public so the JS test
/// runner's bridge reuses the exact capture pipeline.
pub async fn capture_with_options(
  locator: &Locator,
  options: &ScreenshotMatcherOptions,
) -> Result<Vec<u8>, TestFailure> {
  let page = locator.page();
  capture_target(page, Some(locator), options).await
}

/// The same capture for a PAGE subject.
///
/// `expect(page).toHaveScreenshot()` used to call `page.screenshot()`
/// bare, so every capture option — animations, caret, masks, stylePath,
/// clip — was silently dropped on that half of the matcher.
///
/// # Errors
///
/// Forwards the capture failure, with the option that caused it named.
pub async fn capture_page_with_options(
  page: &Arc<ferridriver::Page>,
  options: &ScreenshotMatcherOptions,
) -> Result<Vec<u8>, TestFailure> {
  capture_target(page, None, options).await
}

async fn capture_target(
  page: &Arc<ferridriver::Page>,
  locator: Option<&Locator>,
  options: &ScreenshotMatcherOptions,
) -> Result<Vec<u8>, TestFailure> {
  use ferridriver::options::{AnimationsMode, CaretMode, ScreenshotScale};

  // Every capture option is lowered onto the SAME options core's
  // `page.screenshot()` takes, rather than re-implemented here. The
  // matcher used to inject its own CSS for animations, caret, style and
  // masks: two implementations of one option, and the weaker one. Its
  // mask was a `background`/`color` rule, so a masked element's children
  // painted straight through it, and `scale` had nowhere to go at all.
  let mut style = String::new();
  for style_path in &options.style_path {
    match std::fs::read_to_string(style_path) {
      Ok(content) => style.push_str(&content),
      Err(e) => {
        return Err(TestFailure {
          message: format!("toHaveScreenshot stylePath {} unreadable: {e}", style_path.display()),
          stack: None,
          diff: None,
          screenshot: None,
        });
      },
    }
  }

  let animations = match options.animations.as_deref() {
    Some("disabled") => Some(AnimationsMode::Disabled),
    Some("allow") => Some(AnimationsMode::Allow),
    _ => None,
  };
  // Playwright's default is `hide`; only an explicit `initial` keeps the
  // caret, which is exactly what core's `build_css` encodes.
  let caret = match options.caret.as_deref() {
    Some("initial") => Some(CaretMode::Initial),
    Some("hide") => Some(CaretMode::Hide),
    _ => None,
  };
  let scale = match options.scale.as_deref() {
    Some("css") => Some(ScreenshotScale::Css),
    Some("device") => Some(ScreenshotScale::Device),
    _ => None,
  };

  let raw_png = if let Some(locator) = locator {
    {
      // The element form has no `mask`/`style` of its own on the wire,
      // so the page carries them and the element supplies the bounds.
      let page_opts = ferridriver::options::ScreenshotOptions {
        animations,
        caret,
        mask: options.mask.iter().map(|sel| page.locator(sel)).collect(),
        mask_color: options.mask_color.clone(),
        scale,
        style: (!style.is_empty()).then(|| style.clone()),
        ..Default::default()
      };
      let restore = install_page_capture_state(page, &page_opts).await?;
      let opts = ferridriver::options::ElementScreenshotOptions {
        omit_background: options.omit_background,
        ..Default::default()
      };
      let shot = locator.screenshot().options(opts).await;
      restore.undo(page).await;
      shot
    }
  } else {
    {
      let opts = ferridriver::options::ScreenshotOptions {
        animations,
        caret,
        full_page: options.full_page,
        mask: options.mask.iter().map(|sel| page.locator(sel)).collect(),
        mask_color: options.mask_color.clone(),
        omit_background: options.omit_background,
        scale,
        style: (!style.is_empty()).then_some(style),
        ..Default::default()
      };
      page.screenshot().options(opts).await
    }
  }
  .map_err(|e| TestFailure {
    message: format!("screenshot failed: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  });

  let png = raw_png?;

  if let Some(clip) = options.clip {
    Ok(crop_png_to_clip(&png, &clip)?)
  } else {
    Ok(png)
  }
}

/// The page-level DOM state an ELEMENT capture still needs.
///
/// `locator.screenshot()` takes only the element's own options on every
/// backend, so the style rules and mask overlays that belong to the page
/// are installed around it and torn down after — the same pair core runs
/// inside `page.screenshot()`, driven from here so both subjects reach
/// one implementation.
struct PageCaptureState {
  style: bool,
  mask: bool,
}

impl PageCaptureState {
  async fn undo(self, page: &Arc<ferridriver::Page>) {
    if self.mask {
      let _ = eval_void(page, ferridriver::backend::screenshot_js::uninstall_mask_js()).await;
    }
    if self.style {
      let _ = eval_void(page, ferridriver::backend::screenshot_js::uninstall_style_js()).await;
    }
  }
}

async fn eval_void(page: &Arc<ferridriver::Page>, js: &str) -> Result<(), ferridriver::error::FerriError> {
  page
    .evaluate(js, ferridriver::protocol::SerializedArgument::default(), None)
    .await
    .map(|_| ())
}

async fn install_page_capture_state(
  page: &Arc<ferridriver::Page>,
  opts: &ferridriver::options::ScreenshotOptions,
) -> Result<PageCaptureState, TestFailure> {
  let wire = opts.to_backend_opts();
  let fail = |what: &str, e: ferridriver::error::FerriError| TestFailure {
    message: format!("screenshot capture-options {what} failed: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  };

  let css = ferridriver::backend::screenshot_js::build_css(&wire);
  let mut state = PageCaptureState {
    style: false,
    mask: false,
  };
  if !css.is_empty() {
    eval_void(page, &ferridriver::backend::screenshot_js::install_style_js(&css))
      .await
      .map_err(|e| fail("style", e))?;
    state.style = true;
  }
  if let Some(install) = ferridriver::backend::screenshot_js::install_mask_js(&wire) {
    page.ensure_engine_injected().await.map_err(|e| fail("mask", e))?;
    eval_void(page, &install).await.map_err(|e| fail("mask", e))?;
    state.mask = true;
  }
  Ok(state)
}

fn crop_png_to_clip(png: &[u8], clip: &super::ScreenshotClip) -> Result<Vec<u8>, TestFailure> {
  use image::GenericImageView;

  let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(|e| TestFailure {
    message: format!("toHaveScreenshot clip: failed to decode capture: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  })?;
  let (img_w, img_h) = img.dimensions();
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let x = (clip.x.max(0.0).min(f64::from(img_w))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let y = (clip.y.max(0.0).min(f64::from(img_h))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let w = (clip.width.max(0.0).min(f64::from(img_w.saturating_sub(x)))) as u32;
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  let h = (clip.height.max(0.0).min(f64::from(img_h.saturating_sub(y)))) as u32;
  if w == 0 || h == 0 {
    return Err(TestFailure {
      message: format!(
        "toHaveScreenshot clip: empty rect after clamping (x={x} y={y} w={w} h={h}) against {img_w}x{img_h} capture"
      ),
      stack: None,
      diff: None,
      screenshot: None,
    });
  }
  let cropped = img.crop_imm(x, y, w, h);
  let mut out = Vec::new();
  cropped
    .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
    .map_err(|e| TestFailure {
      message: format!("toHaveScreenshot clip: re-encode failed: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?;
  Ok(out)
}
