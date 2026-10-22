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
  ASYM_TAG_KEY, AssertionFailure, Asymmetric, BUILTIN_MATCHER_NAMES, CallerLocation, DEFAULT_EXPECT_TIMEOUT, Expect,
  ExpectConfigure, ExpectContext, ExpectFn, ExpectMeta, ExpectPoll, ExpectValue, HaveCssOptions, InViewportOptions,
  MatchError, MatcherContext, MatcherResult, MatcherSet, POLL_INTERVALS, StringOrRegex, ThrowMatcher, ThrownError,
  ToPassOptions, TypeTag, ValueMatcher, deep_equal, expect, expect_configured, expect_fn, expect_poll, expect_value,
  finalize, is_builtin_matcher, json_diff, match_object, matcher, pretty_json, run_value_matcher, to_pass,
  to_pass_with_options, unified_diff,
};
pub use locator::LocatorSnapshotMatchers;
pub use page::PageSnapshotMatchers;

/// The `expect` block in force, or Playwright's defaults outside a test.
/// The scope itself is [`ferridriver_expect::with_expect_config`], so
/// the timeout the poll loop reads and the screenshot budget a matcher
/// reads cannot come from different places.
#[must_use]
pub fn current_expect_config() -> std::sync::Arc<crate::config::ExpectConfig> {
  ferridriver_expect::current_expect_config().unwrap_or_default()
}

/// Playwright's `expect.toMatchAriaSnapshot.children`: prepend the
/// configured `/children` mode unless the template already declares one
/// (`matchers/toMatchAriaSnapshot.ts:90-92`).
#[must_use]
pub fn aria_template_with_children(expected_yaml: &str, children: Option<&str>) -> String {
  match children {
    Some(mode) if !expected_yaml.lines().any(|l| l.starts_with("- /children:")) => {
      format!("- /children: {mode}\n{expected_yaml}")
    },
    _ => expected_yaml.to_string(),
  }
}

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
  /// Playwright's private `_comparator`: `pixelmatch` unless set.
  pub comparator: crate::comparator::Comparator,
  pub mask_color: Option<String>,
  pub animations: Option<String>,
  pub caret: Option<String>,
  pub scale: Option<String>,
  /// Playwright spells it `string | string[]`; empty means unset.
  pub style_path: Vec<std::path::PathBuf>,
  pub clip: Option<ScreenshotClip>,
  pub mask: Vec<String>,
  /// Page form only — Playwright puts `fullPage` on
  /// `PageAssertions.toHaveScreenshot`, not the locator's.
  pub full_page: Option<bool>,
  /// Keep the capture transparent where the page paints no background.
  pub omit_background: Option<bool>,
  pub ignore: bool,
  /// `expect.toHaveScreenshot.timeout`, or the per-call `timeout`, or
  /// neither — in which case the assertion's own timeout applies.
  pub timeout: Option<std::time::Duration>,
}

impl ScreenshotMatcherOptions {
  /// Fill in what the CALL did not set from the `expect.toHaveScreenshot`
  /// block — Playwright's `{ ...configOptions, ...callOptions }`
  /// (`matchers/toMatchSnapshot.ts:121-127`).
  ///
  /// `clip`, `mask`, `maskColor`, `fullPage` and `omitBackground` are
  /// per-call only: upstream strips them from the config bag first
  /// (`NonConfigProperties`, `:62`).
  #[must_use]
  pub fn with_config_defaults(mut self, cfg: &crate::config::ToHaveScreenshotConfig) -> Self {
    if self.threshold.is_none() {
      self.threshold = cfg.threshold;
    }
    if self.max_diff_pixels.is_none() {
      self.max_diff_pixels = cfg.max_diff_pixels.map(u64::from);
    }
    if self.max_diff_pixel_ratio.is_none() {
      self.max_diff_pixel_ratio = cfg.max_diff_pixel_ratio;
    }
    if self.animations.is_none() {
      self.animations.clone_from(&cfg.animations);
    }
    if self.caret.is_none() {
      self.caret.clone_from(&cfg.caret);
    }
    if self.scale.is_none() {
      self.scale.clone_from(&cfg.scale);
    }
    if self.style_path.is_empty()
      && let Some(style) = &cfg.style_path
    {
      self.style_path = style.to_vec().into_iter().map(std::path::PathBuf::from).collect();
    }
    if self.timeout.is_none() {
      self.timeout = cfg.timeout.map(std::time::Duration::from_millis);
    }
    self
  }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScreenshotClip {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

/// One assertion as the step it is reported as — Playwright's
/// `callMatcherAsStep` (`matchers/expect.ts`), which opens an `expect`
/// step for every matcher call, value matchers included.
#[derive(Debug, Clone, Default)]
pub struct ExpectStep {
  pub matcher: String,
  pub is_not: bool,
  pub is_soft: bool,
  pub is_poll: bool,
  /// `expect(subject, message)`: replaces the whole title.
  pub message: Option<String>,
  /// The subject's selector, when it is a locator.
  pub selector: Option<String>,
  /// The matcher's first argument, as JSON.
  pub expected: Option<serde_json::Value>,
}

impl ExpectStep {
  /// `Expect "not toBe"`, or the custom message.
  #[must_use]
  pub fn title(&self) -> String {
    if let Some(message) = self.message.as_deref().filter(|m| !m.is_empty()) {
      return message.to_string();
    }
    let mut title = String::new();
    for (on, prefix) in [(self.is_poll, "poll "), (self.is_soft, "soft "), (self.is_not, "not ")] {
      if on {
        title.push_str(prefix);
      }
    }
    title.push_str(&self.matcher);
    if self.matcher == "toHaveScreenshot"
      && let Some(name) = self.expected.as_ref().and_then(screenshot_step_name)
    {
      title.push_str(&format!("({name})"));
    }
    // `escapeWithQuotes(title, '"')` is exactly a JSON string literal.
    format!("Expect {}", serde_json::Value::String(title))
  }

  /// The step spec a host opens for this assertion.
  #[must_use]
  pub fn spec(&self) -> crate::step::StepSpec {
    let locator = self
      .selector
      .as_deref()
      .map(ferridriver::locator_generator::as_locator_description);
    let mut params = serde_json::Map::new();
    if let Some(locator) = &locator {
      params.insert("locator".to_string(), serde_json::Value::String(locator.clone()));
    }
    if let Some(expected) = self.expected.clone().filter(truthy) {
      params.insert("expected".to_string(), expected);
    }
    let mut spec = crate::step::StepSpec::new(self.title()).with_options(crate::step::StepOptions {
      subtitle: locator,
      params: (!params.is_empty()).then_some(serde_json::Value::Object(params)),
      ..crate::step::StepOptions::default()
    });
    spec.category = crate::model::StepCategory::Expect;
    spec
  }
}

/// `if (args[0]) params.expected = args[0]`.
fn truthy(value: &serde_json::Value) -> bool {
  match value {
    serde_json::Value::Null => false,
    serde_json::Value::Bool(b) => *b,
    serde_json::Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
    serde_json::Value::String(s) => !s.is_empty(),
    serde_json::Value::Array(_) | serde_json::Value::Object(_) => true,
  }
}

/// `toHaveScreenshotStepTitle`: the snapshot name, given directly or as
/// `{ name }`, its segments joined with the path separator.
fn screenshot_step_name(first: &serde_json::Value) -> Option<String> {
  let name = match first {
    serde_json::Value::Object(options) => options.get("name")?,
    other => other,
  };
  let name = match name {
    serde_json::Value::String(s) => s.clone(),
    serde_json::Value::Array(parts) => parts
      .iter()
      .filter_map(serde_json::Value::as_str)
      .collect::<Vec<_>>()
      .join(std::path::MAIN_SEPARATOR_STR),
    _ => return None,
  };
  (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod expect_step_tests {
  use serde_json::json;

  use super::ExpectStep;

  fn step(matcher: &str) -> ExpectStep {
    ExpectStep {
      matcher: matcher.to_string(),
      ..ExpectStep::default()
    }
  }

  #[test]
  fn titles_carry_the_modifiers_in_playwright_order() {
    assert_eq!(step("toHaveTitle").title(), r#"Expect "toHaveTitle""#);
    let all = ExpectStep {
      is_not: true,
      is_soft: true,
      is_poll: true,
      ..step("toBe")
    };
    assert_eq!(all.title(), r#"Expect "poll soft not toBe""#);
    let custom = ExpectStep {
      message: Some("the cart total".into()),
      ..step("toBe")
    };
    assert_eq!(custom.title(), "the cart total");
    let shot = ExpectStep {
      expected: Some(json!(["home", "hero.png"])),
      ..step("toHaveScreenshot")
    };
    assert_eq!(
      shot.title(),
      format!("Expect \"toHaveScreenshot(home{}hero.png)\"", std::path::MAIN_SEPARATOR)
    );
  }

  #[test]
  fn a_locator_subject_is_the_subtitle_and_a_falsy_expectation_is_not_a_param() {
    let spec = ExpectStep {
      selector: Some("internal:role=heading".into()),
      expected: Some(json!(0)),
      ..step("toHaveCount")
    }
    .spec();
    assert_eq!(spec.category, crate::model::StepCategory::Expect);
    assert_eq!(spec.options.subtitle.as_deref(), Some("getByRole('heading')"));
    assert_eq!(spec.options.params, Some(json!({ "locator": "getByRole('heading')" })));
  }
}
