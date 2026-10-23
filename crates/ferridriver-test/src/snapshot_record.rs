//! What a snapshot comparison found, as data a report can act on.
//!
//! Playwright reports a screenshot assertion only through its error text
//! and the `-expected`, `-actual` and `-diff` attachments. A reviewer
//! that has to tell a changed screenshot from a new one, or show how many
//! pixels moved, would have to parse that text. Every `toHaveScreenshot`
//! and `toMatchSnapshot` call therefore also leaves one
//! [`SnapshotComparison`] on the attempt, carried on the wire beside the
//! attachments it names.

use serde::{Deserialize, Serialize};

/// Which matcher compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotComparisonKind {
  /// `toHaveScreenshot`.
  Screenshot,
  /// `toMatchSnapshot`.
  Snapshot,
}

/// How the comparison ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotStatus {
  /// Matched the baseline within the budget.
  Passed,
  /// Differed beyond the budget, and the assertion failed.
  Changed,
  /// No baseline, and none was written.
  Missing,
  /// No baseline; the actual value became it.
  Written,
  /// A baseline existed and differed; the update mode rewrote it.
  Updated,
}

/// One comparison of one attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotComparison {
  /// The attachment base name (`hero.png`), the snapshot's path relative
  /// to the test's output directory.
  pub name: String,
  pub kind: SnapshotComparisonKind,
  pub status: SnapshotStatus,
  /// Where the baseline lives (or would have).
  pub snapshot_path: String,
  /// Whether this comparison failed the test, outright or softly.
  pub failed: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub message: Option<String>,
  /// Names of attachments on the same outcome.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub expected: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub actual: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub diff: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub previous: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub diff_pixels: Option<u64>,
  /// `diff_pixels` over the expected image's pixels, unrounded.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub diff_ratio: Option<f64>,
  /// The actual image's size.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub width: Option<u32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub height: Option<u32>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub threshold: Option<f64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_diff_pixels: Option<u64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub max_diff_pixel_ratio: Option<f64>,
}

impl SnapshotComparison {
  /// A comparison of `kind` against `snapshot_path` with nothing found yet.
  #[must_use]
  pub fn new(name: String, kind: SnapshotComparisonKind, snapshot_path: String, status: SnapshotStatus) -> Self {
    Self {
      name,
      kind,
      status,
      snapshot_path,
      failed: false,
      message: None,
      expected: None,
      actual: None,
      diff: None,
      previous: None,
      diff_pixels: None,
      diff_ratio: None,
      width: None,
      height: None,
      threshold: None,
      max_diff_pixels: None,
      max_diff_pixel_ratio: None,
    }
  }

  /// Fill in the budget the comparison ran under.
  #[must_use]
  pub fn with_budget(mut self, options: &crate::comparator::ImageComparatorOptions) -> Self {
    self.threshold = options.threshold;
    self.max_diff_pixels = options.max_diff_pixels;
    self.max_diff_pixel_ratio = options.max_diff_pixel_ratio;
    self
  }

  /// Fill in what an image comparison measured.
  pub fn measured(&mut self, comparison: &crate::comparator::ImageComparison) {
    self.diff_pixels = Some(comparison.diff_pixels);
    self.diff_ratio = Some(comparison.diff_ratio());
    self.width = Some(comparison.width);
    self.height = Some(comparison.height);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn absent_fields_stay_off_the_wire_and_read_back() {
    let mut record = SnapshotComparison::new(
      "hero.png".into(),
      SnapshotComparisonKind::Screenshot,
      "/base/hero.png".into(),
      SnapshotStatus::Changed,
    );
    record.failed = true;
    record.actual = Some("hero-actual.png".into());
    record.diff_ratio = Some(0.25);
    let json = serde_json::to_value(&record).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "name": "hero.png", "kind": "screenshot", "status": "changed", "snapshot_path": "/base/hero.png",
        "failed": true, "actual": "hero-actual.png", "diff_ratio": 0.25,
      })
    );
    let back: SnapshotComparison = serde_json::from_value(json).unwrap();
    assert_eq!(back, record);
  }
}
