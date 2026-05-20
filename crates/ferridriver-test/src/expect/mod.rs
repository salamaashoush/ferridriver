//! Auto-retrying assertions. The full matcher set — builder, value
//! matchers, asymmetric matchers, locator / page / `APIResponse`
//! web-first matchers, polling, and `toPass` — lives in
//! [`ferridriver_expect`]. The thin wrappers in this module:
//!
//! 1. Re-export every public symbol so existing call sites keep working
//!    against `ferridriver_test::expect::*`.
//! 2. Adapt the lightweight `AssertionFailure` to the test runner's
//!    richer `TestFailure` (which carries screenshots + structured
//!    stack frames).
//! 3. Host the screenshot / aria-snapshot / value-snapshot matchers
//!    that need the test runner's `TestInfo` + image / aria-YAML
//!    plumbing.
//!
//! ```ignore
//! use ferridriver_test::expect::expect;
//!
//! expect(&page).to_have_title("Example").await?;
//! expect(&page.locator("h1")).to_have_text("Hello").await?;
//! expect_value(json!({"id":1})).to_equal(&json!({"id":1}))?;
//! ```

pub mod locator;
pub mod page;
pub mod value;

pub use ferridriver_expect::{
  ASYM_TAG_KEY, AssertionFailure, Asymmetric, CallerLocation, DEFAULT_EXPECT_TIMEOUT, Expect, ExpectContext, ExpectFn,
  ExpectPoll, ExpectValue, HaveCssOptions, InViewportOptions, MatchError, POLL_INTERVALS, StringOrRegex, ThrowMatcher,
  ThrownError, ToPassOptions, TypeTag, deep_equal, expect, expect_configured, expect_fn, expect_poll, expect_value,
  json_diff, match_object, pretty_json, to_pass, to_pass_with_options, unified_diff,
};
pub use locator::LocatorSnapshotMatchers;
pub use page::PageSnapshotMatchers;

/// Options for `expect(locator|page).toHaveScreenshot()`.
///
/// Mirrors Playwright's `LocatorAssertions.toHaveScreenshot` /
/// `PageAssertions.toHaveScreenshot` option bag. Lives in the test
/// crate because the underlying screenshot pipeline reads
/// `TestInfo.snapshot_dir` and writes baseline PNGs.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotMatcherOptions {
  pub threshold: Option<f64>,
  pub max_diff_pixels: Option<u64>,
  pub max_diff_pixel_ratio: Option<f64>,
  pub mask_color: Option<String>,
  pub animations: Option<String>,
  pub caret: Option<String>,
  pub scale: Option<String>,
  pub style_path: Option<std::path::PathBuf>,
  pub clip: Option<ScreenshotClip>,
  pub mask: Vec<String>,
  pub ignore: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScreenshotClip {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}
