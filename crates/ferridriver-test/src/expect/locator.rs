//! Snapshot / screenshot / aria matchers for `Expect<Locator>`. These
//! stay in the test runner because they need `TestInfo`-keyed snapshot
//! directories, the `image` crate, and the Playwright-bundled aria
//! renderer's YAML output format. Every other locator matcher lives in
//! [`ferridriver_expect`] (single source of truth).

use std::future::Future;
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

  let mut style_blocks: Vec<String> = Vec::new();

  if options.animations.as_deref() == Some("disabled") {
    style_blocks.push(
      "*, *::before, *::after { \
        animation-duration: 0s !important; \
        animation-delay: 0s !important; \
        animation-iteration-count: 1 !important; \
        transition-duration: 0s !important; \
        transition-delay: 0s !important; \
      }"
      .to_string(),
    );
  }

  if options.caret.as_deref() == Some("hide") {
    style_blocks.push("html, body, * { caret-color: transparent !important; }".to_string());
  }

  for style_path in &options.style_path {
    match std::fs::read_to_string(style_path) {
      Ok(content) => style_blocks.push(content),
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

  let mask_color = options.mask_color.as_deref().unwrap_or("#FF00FF");
  if !options.mask.is_empty() {
    let mut mask_css = String::new();
    for selector in &options.mask {
      mask_css.push_str(selector);
      mask_css.push_str(" { background: ");
      mask_css.push_str(mask_color);
      mask_css.push_str(" !important; color: ");
      mask_css.push_str(mask_color);
      mask_css.push_str(" !important; }\n");
    }
    style_blocks.push(mask_css);
  }

  let token = "ferridriver-screenshot-capture";

  if !style_blocks.is_empty() {
    let combined = style_blocks.join("\n");
    let escaped = serde_json::to_string(&combined).unwrap_or_else(|_| "\"\"".to_string());
    let inject_script = format!(
      "(function() {{ \
        const s = document.createElement('style'); \
        s.setAttribute('data-{TOK}', '1'); \
        s.textContent = {ESC}; \
        document.head.appendChild(s); \
        return true; \
      }})()",
      TOK = token,
      ESC = escaped,
    );
    let _ = page
      .evaluate(
        &inject_script,
        ferridriver::protocol::SerializedArgument::default(),
        None,
      )
      .await
      .map_err(|e| TestFailure {
        message: format!("screenshot capture-options inject failed: {e}"),
        stack: None,
        diff: None,
        screenshot: None,
      })?;
  }

  let raw_png = locator.screenshot().await.map_err(|e| TestFailure {
    message: format!("screenshot failed: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  });

  if !style_blocks.is_empty() {
    let cleanup = format!(
      "(function() {{ \
        document.querySelectorAll('style[data-{TOK}]').forEach(function(n) {{ n.remove(); }}); \
        return true; \
      }})()",
      TOK = token,
    );
    let _ = page
      .evaluate(&cleanup, ferridriver::protocol::SerializedArgument::default(), None)
      .await;
  }

  let png = raw_png?;

  if let Some(clip) = options.clip {
    Ok(crop_png_to_clip(&png, &clip)?)
  } else {
    Ok(png)
  }
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
