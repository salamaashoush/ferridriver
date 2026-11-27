//! Trace, video, and screenshot recording modes and option deserializers.

use crate::browser::ViewportConfig;
use serde::{Deserialize, Serialize};

// ── Trace mode ──────────────────────────────────────────────────────────────

/// Trace recording mode. Mirrors Playwright's `trace`.
/// Playwright's `TraceMode` (`playwright/types/test.d.ts:7240`), whole.
///
/// The five conditional modes differ only in WHEN they record and WHEN
/// they keep what they recorded; both questions are answered here so no
/// caller re-derives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceMode {
  #[default]
  Off,
  On,
  RetainOnFailure,
  /// `retry-with-trace` is upstream's deprecated spelling of this
  /// (`playwright/src/worker/testTracing.ts:100`).
  #[serde(alias = "retry-with-trace")]
  OnFirstRetry,
  OnAllRetries,
  RetainOnFirstFailure,
  RetainOnFailureAndRetries,
}

impl TraceMode {
  /// Parse from string (config/CLI). An unknown label is `Off`.
  #[must_use]
  pub fn parse_label(s: &str) -> Self {
    match s {
      "on" => Self::On,
      "retain-on-failure" => Self::RetainOnFailure,
      "on-first-retry" | "retry-with-trace" => Self::OnFirstRetry,
      "on-all-retries" => Self::OnAllRetries,
      "retain-on-first-failure" => Self::RetainOnFirstFailure,
      "retain-on-failure-and-retries" => Self::RetainOnFailureAndRetries,
      _ => Self::Off,
    }
  }

  /// Should we record for this test attempt?
  ///
  /// `attempt` is 1-based; upstream's `testInfo.retry` is `attempt - 1`
  /// (`playwright/src/worker/testTracing.ts::_shouldCaptureTrace`).
  #[must_use]
  pub fn should_record(self, attempt: u32, _failed: bool) -> bool {
    let retry = attempt.saturating_sub(1);
    match self {
      Self::Off => false,
      Self::On | Self::RetainOnFailure | Self::RetainOnFailureAndRetries => true,
      Self::OnFirstRetry => retry == 1,
      Self::OnAllRetries => retry > 0,
      Self::RetainOnFirstFailure => retry == 0,
    }
  }

  /// Should we keep the trace after the test finished?
  ///
  /// Upstream spells this as `_shouldAbandonTrace`
  /// (`playwright/src/worker/testTracing.ts:164-171`); this is its
  /// negation.
  #[must_use]
  pub fn should_retain(self, failed: bool, attempt: u32) -> bool {
    let retry = attempt.saturating_sub(1);
    match self {
      Self::Off => false,
      Self::On | Self::OnFirstRetry | Self::OnAllRetries => true,
      Self::RetainOnFailure | Self::RetainOnFirstFailure => failed,
      Self::RetainOnFailureAndRetries => failed || retry > 0,
    }
  }

  /// Combined check: should we actually write a trace file?
  #[must_use]
  pub fn should_write(self, attempt: u32, failed: bool) -> bool {
    self.should_record(attempt, failed) && self.should_retain(failed, attempt)
  }
}

// ── Video ───────────────────────────────────────────────────────────────────

/// Playwright's `VideoMode` (`playwright/types/test.d.ts:7241`) — the
/// same seven states `TraceMode` has, decided by the same two rules
/// (`playwright/src/index.ts::shouldCaptureVideo` / `shouldPreserveVideo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoMode {
  #[default]
  Off,
  On,
  RetainOnFailure,
  /// `retry-with-video` is upstream's deprecated spelling
  /// (`playwright/src/index.ts::normalizeVideoMode`).
  #[serde(alias = "retry-with-video")]
  OnFirstRetry,
  OnAllRetries,
  RetainOnFirstFailure,
  RetainOnFailureAndRetries,
}

impl VideoMode {
  /// Parse from string (config/CLI). An unknown label is `Off`.
  #[must_use]
  pub fn parse_label(s: &str) -> Self {
    match s {
      "on" => Self::On,
      "retain-on-failure" => Self::RetainOnFailure,
      "on-first-retry" | "retry-with-video" => Self::OnFirstRetry,
      "on-all-retries" => Self::OnAllRetries,
      "retain-on-first-failure" => Self::RetainOnFirstFailure,
      "retain-on-failure-and-retries" => Self::RetainOnFailureAndRetries,
      _ => Self::Off,
    }
  }

  /// Should this attempt record at all? `attempt` is 1-based.
  #[must_use]
  pub fn should_record(self, attempt: u32) -> bool {
    let retry = attempt.saturating_sub(1);
    match self {
      Self::Off => false,
      Self::On | Self::RetainOnFailure | Self::RetainOnFailureAndRetries => true,
      Self::OnFirstRetry => retry == 1,
      Self::OnAllRetries => retry > 0,
      Self::RetainOnFirstFailure => retry == 0,
    }
  }

  /// Should the recording be kept once the test finished?
  #[must_use]
  pub fn should_retain(self, failed: bool, attempt: u32) -> bool {
    let retry = attempt.saturating_sub(1);
    match self {
      Self::Off => false,
      Self::On | Self::OnFirstRetry | Self::OnAllRetries => true,
      Self::RetainOnFailure | Self::RetainOnFirstFailure => failed,
      Self::RetainOnFailureAndRetries => failed || retry > 0,
    }
  }

  /// Record straight to disk rather than into a ring buffer.
  ///
  /// True exactly when the recording is kept even if the test PASSES —
  /// then there is nothing to decide at the end and buffering only costs
  /// memory. The buffered path exists for the modes that keep a video
  /// only on failure.
  #[must_use]
  pub fn records_eagerly(self, attempt: u32) -> bool {
    self.should_retain(false, attempt)
  }
}

/// Video recording configuration: a mode (`video = "on"`), or a mode with
/// the frame size. Without a size a video takes the page's viewport, scaled
/// down to fit 800x800.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoConfig {
  pub mode: VideoMode,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub width: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub height: Option<u32>,
}

impl VideoConfig {
  /// The frame size for a page whose viewport is `viewport`: the size
  /// given, else the viewport scaled down to fit 800x800 (800x600 for a
  /// page with none), each side even, as video encoders need.
  #[must_use]
  pub fn frame_size(&self, viewport: Option<(i64, i64)>) -> (u32, u32) {
    let (width, height) = match (self.width, self.height, viewport) {
      (Some(width), Some(height), _) => (width, height),
      (_, _, Some((width, height))) if width > 0 && height > 0 => {
        let (width, height) = (width.unsigned_abs(), height.unsigned_abs());
        let longest = width.max(height);
        let fit = |side: u64| {
          let scaled = if longest > 800 { side * 800 / longest } else { side };
          u32::try_from(scaled).unwrap_or(800)
        };
        (fit(width), fit(height))
      },
      _ => (800, 600),
    };
    ((width & !1).max(2), (height & !1).max(2))
  }
}

/// Playwright's `ScreenshotMode` (`playwright/types/test.d.ts:7239`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScreenshotMode {
  #[default]
  Off,
  On,
  OnlyOnFailure,
  OnFirstFailure,
}

impl ScreenshotMode {
  /// Parse from string (config/CLI). An unknown label is `Off`.
  #[must_use]
  pub fn parse_label(s: &str) -> Self {
    match s {
      "on" => Self::On,
      "only-on-failure" => Self::OnlyOnFailure,
      "on-first-failure" => Self::OnFirstFailure,
      _ => Self::Off,
    }
  }

  /// Should this attempt end with a screenshot? `attempt` is 1-based.
  #[must_use]
  pub fn should_capture(self, failed: bool, attempt: u32) -> bool {
    match self {
      Self::Off => false,
      Self::On => true,
      Self::OnlyOnFailure => failed,
      Self::OnFirstFailure => failed && attempt.saturating_sub(1) == 0,
    }
  }
}

/// `use: { screenshot }` — a mode, or a mode with capture options
/// (`ScreenshotMode | { mode } & Pick<PageScreenshotOptions, 'fullPage' |
/// 'omitBackground'>`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotOption {
  pub mode: ScreenshotMode,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub full_page: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub omit_background: Option<bool>,
}

/// `use: { video }` — a mode, or a mode with a recording size
/// (`VideoMode | { mode, size? }`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoOption {
  pub mode: VideoMode,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub size: Option<ViewportConfig>,
}

/// `use: { trace }` — a mode, or a mode with what to record
/// (`TraceMode | { mode, screenshots?, snapshots?, sources?,
/// attachments? }`).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceOption {
  pub mode: TraceMode,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub screenshots: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub snapshots: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub sources: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub attachments: Option<bool>,
}

// Each of the three is `mode | { mode, ... }`. Hand-written rather than
// `#[serde(untagged)]`: an untagged enum reports every mistake as "did
// not match any variant", so a misspelled mode inside the object form
// would stop naming the mode.
macro_rules! mode_or_object {
  ($option:ty, $fields:ty, $mode:ty) => {
    impl<'de> Deserialize<'de> for $option {
      fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(de).map_err(serde::de::Error::custom)?;
        if let Some(label) = value.as_str() {
          return Ok(Self {
            mode: <$mode>::parse_label(label),
            ..Self::default()
          });
        }
        serde_json::from_value::<$fields>(value)
          .map(Into::into)
          .map_err(serde::de::Error::custom)
      }
    }
  };
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenshotFields {
  #[serde(default)]
  mode: ScreenshotMode,
  full_page: Option<bool>,
  omit_background: Option<bool>,
}

impl From<ScreenshotFields> for ScreenshotOption {
  fn from(f: ScreenshotFields) -> Self {
    Self {
      mode: f.mode,
      full_page: f.full_page,
      omit_background: f.omit_background,
    }
  }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VideoFields {
  #[serde(default)]
  mode: VideoMode,
  size: Option<ViewportConfig>,
}

impl From<VideoFields> for VideoOption {
  fn from(f: VideoFields) -> Self {
    Self {
      mode: f.mode,
      size: f.size,
    }
  }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TraceFields {
  #[serde(default)]
  mode: TraceMode,
  screenshots: Option<bool>,
  snapshots: Option<bool>,
  sources: Option<bool>,
  attachments: Option<bool>,
}

impl From<TraceFields> for TraceOption {
  fn from(f: TraceFields) -> Self {
    Self {
      mode: f.mode,
      screenshots: f.screenshots,
      snapshots: f.snapshots,
      sources: f.sources,
      attachments: f.attachments,
    }
  }
}

mode_or_object!(ScreenshotOption, ScreenshotFields, ScreenshotMode);
mode_or_object!(VideoOption, VideoFields, VideoMode);
mode_or_object!(TraceOption, TraceFields, TraceMode);

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct VideoConfigFields {
  mode: VideoMode,
  width: Option<u32>,
  height: Option<u32>,
}

impl From<VideoConfigFields> for VideoConfig {
  fn from(f: VideoConfigFields) -> Self {
    Self {
      mode: f.mode,
      width: f.width,
      height: f.height,
    }
  }
}

mode_or_object!(VideoConfig, VideoConfigFields, VideoMode);
