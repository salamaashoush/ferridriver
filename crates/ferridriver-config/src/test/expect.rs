//! Assertion (`expect`) configuration and worker count types.

use serde::{Deserialize, Serialize};

// ── Expect config ───────────────────────────────────────────────────────────

/// Playwright's default `expect()` timeout when nothing sets one.
pub const DEFAULT_EXPECT_TIMEOUT_MS: u64 = 5_000;

/// The `expect` block, on both `TestConfig` and `ProjectConfig`
/// (`playwright/types/test.d.ts:184` and `:1131`).
///
/// A project's block REPLACES the config's whole object rather than
/// merging into it — `takeFirst(projectConfig.expect, config.expect,
/// {})` (`playwright/src/common/config.ts:201`) — so a project that
/// sets only `timeout` starts from Playwright's defaults for
/// everything else, not from the config's screenshot thresholds. See
/// [`TestConfig::resolved_expect`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExpectConfig {
  /// Default timeout for auto-retrying matchers, in ms.
  pub timeout: Option<u64>,
  pub to_have_screenshot: ToHaveScreenshotConfig,
  pub to_match_snapshot: ToMatchSnapshotConfig,
  pub to_match_aria_snapshot: ToMatchAriaSnapshotConfig,
  pub to_pass: ToPassConfig,
}

impl ExpectConfig {
  /// The timeout matchers start from, defaulted the way Playwright does.
  #[must_use]
  pub fn timeout_ms(&self) -> u64 {
    self.timeout.unwrap_or(DEFAULT_EXPECT_TIMEOUT_MS)
  }
}

/// `expect.toHaveScreenshot` defaults. Every key but `mask` and
/// `maskColor` is Playwright's; the per-call option bag layers on top of
/// whatever is set here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToHaveScreenshotConfig {
  pub threshold: Option<f64>,
  pub max_diff_pixels: Option<u32>,
  pub max_diff_pixel_ratio: Option<f64>,
  pub animations: Option<String>,
  pub caret: Option<String>,
  pub scale: Option<String>,
  /// Stylesheet(s) applied before the screenshot. Relative entries are
  /// anchored to the directory of the config file that declared them
  /// (Playwright resolves against `configDir`).
  pub style_path: Option<StringOrList>,
  /// Consumed by the snapshot path resolver.
  pub path_template: Option<String>,
  /// Overrides `expect.timeout` for this matcher only.
  pub timeout: Option<u64>,
  /// Selectors masked in every screenshot, beside any a call masks.
  /// Playwright keeps `mask` out of config because a `Locator` does not
  /// serialize; a selector does, and dynamic regions (clocks, ads) are
  /// usually the same on every page of a suite.
  pub mask: Option<Vec<String>>,
  /// The masks' colour when a call names none.
  pub mask_color: Option<String>,
}

/// `expect.toMatchSnapshot` defaults (`types/test.d.ts:279`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToMatchSnapshotConfig {
  pub threshold: Option<f64>,
  pub max_diff_pixels: Option<u32>,
  pub max_diff_pixel_ratio: Option<f64>,
}

/// `expect.toMatchAriaSnapshot` defaults (`types/test.d.ts:258`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToMatchAriaSnapshotConfig {
  pub path_template: Option<String>,
  /// `contain` | `equal` | `deep-equal` — the `/children` property
  /// every template gets unless it declares its own.
  pub children: Option<String>,
}

/// `expect.toPass` defaults (`types/test.d.ts:302`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToPassConfig {
  pub timeout: Option<u64>,
  pub intervals: Option<Vec<u64>>,
}

/// A config value Playwright spells `string | string[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrList {
  One(String),
  Many(Vec<String>),
}

impl StringOrList {
  #[must_use]
  pub fn to_vec(&self) -> Vec<String> {
    match self {
      Self::One(s) => vec![s.clone()],
      Self::Many(v) => v.clone(),
    }
  }
}

// ── Workers ─────────────────────────────────────────────────────────────────

/// Playwright's `resolveWorkers` (`common/config.ts`): a worker count
/// written as a number, a numeric string, or a percentage of logical
/// cores, resolved to a count of at least one.
///
/// # Errors
/// When the value is neither a number nor a percentage, or is below 1.
pub fn parse_workers(text: &str) -> Result<u32, String> {
  let trimmed = text.trim();
  if let Some(percent) = trimmed.strip_suffix('%') {
    let percent = leading_int(percent).ok_or_else(|| format!("Workers {text} must be a number or percentage."))?;
    if percent < 1 {
      return Err(format!("Workers must be a positive number, received {percent}."));
    }
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let cpus = i64::try_from(cpus).unwrap_or(i64::MAX);
    return Ok(u32::try_from((cpus.saturating_mul(percent) / 100).max(1)).unwrap_or(u32::MAX));
  }
  let count = leading_int(trimmed).ok_or_else(|| format!("Workers {text} must be a number or percentage."))?;
  positive_workers(count)
}

/// Playwright's default, `'50%'`: half the logical cores, at least one.
#[must_use]
pub fn default_workers() -> u32 {
  let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
  u32::try_from((cpus / 2).max(1)).unwrap_or(u32::MAX)
}

fn positive_workers(count: i64) -> Result<u32, String> {
  if count < 1 {
    return Err(format!("Workers must be a positive number, received {count}."));
  }
  Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

/// `parseInt`'s reading of a decimal prefix: optional sign, then digits,
/// anything after ignored. `None` when there are no digits (`NaN`).
fn leading_int(text: &str) -> Option<i64> {
  let text = text.trim_start();
  let (sign, rest) = match text.strip_prefix('-') {
    Some(rest) => (-1, rest),
    None => (1, text.strip_prefix('+').unwrap_or(text)),
  };
  let digits = rest.split(|c: char| !c.is_ascii_digit()).next().unwrap_or_default();
  if digits.is_empty() {
    return None;
  }
  Some(sign * digits.parse::<i64>().unwrap_or(i64::MAX))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WorkersValue {
  Count(i64),
  Fraction(f64),
  Text(String),
}

impl WorkersValue {
  fn resolve(self) -> Result<u32, String> {
    match self {
      Self::Count(n) => positive_workers(n),
      Self::Fraction(n) => Err(format!("Workers must be a whole number, received {n}.")),
      Self::Text(text) => parse_workers(&text),
    }
  }
}

pub(super) fn deserialize_optional_workers<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<u32>, D::Error> {
  Option::<WorkersValue>::deserialize(deserializer)?
    .map(WorkersValue::resolve)
    .transpose()
    .map_err(serde::de::Error::custom)
}
