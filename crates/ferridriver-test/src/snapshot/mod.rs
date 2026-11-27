//! Snapshot assertions: `toMatchSnapshot` on a value and the baseline
//! side of `toHaveScreenshot`, with Playwright's update modes
//! (`matchers/toMatchSnapshot.ts`).

use std::fmt::Write as _;
use std::path::Path;

use crate::comparator::{ImageComparatorOptions, ImageComparison, ImageFormat, measure_images};
use crate::model::{Attachment, AttachmentBody, TestFailure, TestInfo};
use crate::snapshot_record::{SnapshotComparison, SnapshotComparisonKind, SnapshotStatus};

/// What `expect(value).toMatchSnapshot()` received: Playwright's
/// `Buffer | string`.
#[derive(Debug, Clone)]
pub enum SnapshotValue {
  Text(String),
  Bytes(Vec<u8>),
}

impl SnapshotValue {
  /// `determineFileExtension`: the extension an unnamed snapshot of this
  /// value gets, from its type or its magic bytes.
  #[must_use]
  pub fn anonymous_extension(&self) -> &'static str {
    match self {
      Self::Text(_) => ".txt",
      Self::Bytes(b) if b.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) => ".png",
      Self::Bytes(b) if b.starts_with(&[0xff, 0xd8, 0xff]) => ".jpg",
      Self::Bytes(b) if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" => ".webp",
      Self::Bytes(_) => ".dat",
    }
  }

  fn bytes(&self) -> &[u8] {
    match self {
      Self::Text(text) => text.as_bytes(),
      Self::Bytes(bytes) => bytes,
    }
  }
}

/// What a snapshot matcher decided, the comparison a report shows, and
/// the files that explain it, in the order Playwright attaches them.
#[derive(Debug)]
pub struct Judged {
  /// `Ok(Some(message))` is a pass that must still fail the test softly:
  /// the baseline was missing under `updateSnapshots: 'default'` and has
  /// just been written.
  pub result: Result<Option<String>, TestFailure>,
  /// `None` for `.not` matchers and ignored snapshots, which compare
  /// nothing worth reviewing.
  pub comparison: Option<SnapshotComparison>,
  pub attachments: Vec<Attachment>,
}

impl Judged {
  fn bare(result: Result<Option<String>, TestFailure>) -> Self {
    Self {
      result,
      comparison: None,
      attachments: Vec::new(),
    }
  }
}

/// `getMimeTypeForPath` for the extensions a snapshot takes.
pub(super) fn mime_type(path: &Path) -> &'static str {
  match path
    .extension()
    .and_then(|e| e.to_str())
    .map(str::to_ascii_lowercase)
    .as_deref()
  {
    Some("png") => "image/png",
    Some("jpg" | "jpeg") => "image/jpeg",
    Some("webp") => "image/webp",
    Some("txt") => "text/plain",
    Some("json") => "application/json",
    _ => "application/octet-stream",
  }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Evidence {
  Expected,
  Previous,
  Actual,
  Diff,
}

/// The record of one comparison as it is being decided, and the files
/// written beside the test's output to explain it.
pub(super) struct Report<'a> {
  files: &'a ScreenshotFiles,
  record: SnapshotComparison,
  attachments: Vec<Attachment>,
}

impl<'a> Report<'a> {
  pub(super) fn new(
    files: &'a ScreenshotFiles,
    kind: SnapshotComparisonKind,
    options: &ImageComparatorOptions,
  ) -> Self {
    Self {
      files,
      record: SnapshotComparison::new(
        files.name.clone(),
        kind,
        files.baseline.display().to_string(),
        SnapshotStatus::Passed,
      )
      .with_budget(options),
      attachments: Vec::new(),
    }
  }

  pub(super) fn status(&mut self, status: SnapshotStatus, failed: bool, message: Option<String>) {
    self.record.status = status;
    self.record.failed = failed;
    self.record.message = message;
  }

  pub(super) fn measured(&mut self, comparison: Option<&ImageComparison>) {
    if let Some(comparison) = comparison {
      self.record.measured(comparison);
    }
  }

  /// Writes `bytes` to the evidence file and attaches it under
  /// Playwright's name for it. A file that cannot be written is left out
  /// rather than attached pointing nowhere.
  pub(super) fn keep(&mut self, evidence: Evidence, bytes: &[u8]) {
    let (path, suffix) = match evidence {
      Evidence::Expected => (&self.files.expected, "-expected"),
      Evidence::Previous => (&self.files.previous, "-previous"),
      Evidence::Actual => (&self.files.actual, "-actual"),
      Evidence::Diff => (&self.files.diff, "-diff"),
    };
    if let Some(parent) = path.parent() {
      let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(path, bytes) {
      tracing::warn!(target: "ferridriver::snapshot", "could not write {}: {e}", path.display());
      return;
    }
    let name = crate::snapshot_path::add_suffix_to_file_path(&self.files.name, suffix);
    let slot = match evidence {
      Evidence::Expected => &mut self.record.expected,
      Evidence::Previous => &mut self.record.previous,
      Evidence::Actual => &mut self.record.actual,
      Evidence::Diff => &mut self.record.diff,
    };
    *slot = Some(name.clone());
    self.attachments.push(Attachment {
      name,
      content_type: mime_type(&self.files.baseline).to_string(),
      body: AttachmentBody::Path(path.clone()),
      step_id: None,
    });
  }

  /// A baseline that did not exist, handled by `handle_missing`: written
  /// (and attached as the expectation) unless the mode refused, the actual
  /// attached either way.
  pub(super) fn missing(&mut self, actual: &[u8], result: &Result<Option<String>, TestFailure>) {
    match result {
      Ok(soft) => {
        self.status(SnapshotStatus::Written, soft.is_some(), soft.clone());
        self.keep(Evidence::Expected, actual);
      },
      Err(failure) => self.status(SnapshotStatus::Missing, true, Some(failure.message.clone())),
    }
    self.keep(Evidence::Actual, actual);
  }

  pub(super) fn finish(self, result: Result<Option<String>, TestFailure>) -> Judged {
    Judged {
      result,
      comparison: Some(self.record),
      attachments: self.attachments,
    }
  }
}

/// Why a value does not match its baseline, and the diff image when the
/// baseline is an image.
struct Difference {
  message: String,
  diff: Option<Vec<u8>>,
}

/// `getComparator(mimeType)` applied to one value and its baseline, with
/// what an image comparison measured.
fn compare_value(
  path: &Path,
  received: &SnapshotValue,
  expected: &[u8],
  options: &ImageComparatorOptions,
) -> Result<(Option<Difference>, Option<ImageComparison>), TestFailure> {
  if let Some(format) = ImageFormat::from_path(path) {
    let SnapshotValue::Bytes(actual) = received else {
      return Ok((
        Some(Difference {
          message: "Actual result should be a Buffer.".to_string(),
          diff: None,
        }),
        None,
      ));
    };
    let mut comparison = measure_images(format, actual, expected, options).map_err(snapshot_failure)?;
    let difference = comparison.mismatch.take().map(|m| Difference {
      message: m.message,
      diff: Some(m.diff),
    });
    return Ok((difference, Some(comparison)));
  }
  let is_text = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"));
  let difference = match received {
    SnapshotValue::Text(actual) => compare_text(actual, &String::from_utf8_lossy(expected)),
    SnapshotValue::Bytes(_) if is_text => Some(Difference {
      message: "Actual result should be a string".to_string(),
      diff: None,
    }),
    SnapshotValue::Bytes(actual) => (actual.as_slice() != expected).then(|| Difference {
      message: "Buffers differ".to_string(),
      diff: None,
    }),
  };
  Ok((difference, None))
}

/// `compareText`: a unified diff with five lines of context.
fn compare_text(actual: &str, expected: &str) -> Option<Difference> {
  if actual == expected {
    return None;
  }
  let with_newline = |s: &str| {
    if s.ends_with('\n') {
      s.to_string()
    } else {
      format!("{s}\n")
    }
  };
  let (expected, actual) = (with_newline(expected), with_newline(actual));
  let diff = similar::TextDiff::from_lines(&expected, &actual);
  let patch = diff.unified_diff().context_radius(5).to_string();
  Some(Difference {
    message: patch,
    diff: None,
  })
}

/// `expect(value).toMatchSnapshot(name?, options?)`.
///
/// The judgement's result is `Ok(Some(message))` for a pass that must
/// still fail the test softly: the baseline was missing under
/// `updateSnapshots: 'default'` and has just been written. A failure
/// carries the comparison, with the actual value and any diff written
/// into the test's output directory and attached.
#[must_use]
pub fn match_value_snapshot(
  test_info: &TestInfo,
  received: &SnapshotValue,
  name: &crate::snapshot_path::SnapshotName,
  options: &ImageComparatorOptions,
  is_not: bool,
) -> Judged {
  if test_info.ignore_snapshots {
    return Judged::bare(Ok(None));
  }
  let resolved = test_info.resolve_snapshot_paths_with_extension(
    crate::snapshot_path::SnapshotKind::Snapshot,
    name,
    true,
    Some(received.anonymous_extension()),
  );
  let files = ScreenshotFiles::new(
    resolved.absolute_snapshot_path.clone(),
    &test_info.output_dir.join(&resolved.relative_output_path),
    resolved.relative_output_path.to_string_lossy().into_owned(),
  );
  if is_not {
    return Judged::bare(match_value_negated(
      &files.baseline,
      received,
      options,
      test_info.update_snapshots,
    ));
  }
  let mut report = Report::new(&files, SnapshotComparisonKind::Snapshot, options);
  match judge_value(&mut report, received, options, test_info.update_snapshots) {
    Ok(result) => report.finish(result),
    Err(failure) => Judged::bare(Err(failure)),
  }
}

fn match_value_negated(
  path: &Path,
  received: &SnapshotValue,
  options: &ImageComparatorOptions,
  mode: crate::config::UpdateSnapshotsMode,
) -> Result<Option<String>, TestFailure> {
  use crate::config::UpdateSnapshotsMode;
  if !path.exists() {
    let tail = if mode == UpdateSnapshotsMode::None {
      "."
    } else {
      ", matchers using \".not\" won't write them automatically."
    };
    return Err(snapshot_failure(format!(
      "A snapshot doesn't exist at {}{tail}",
      path.display()
    )));
  }
  let expected = read_baseline(path)?;
  if compare_value(path, received, &expected, options)?.0.is_some() {
    Ok(None)
  } else {
    Err(snapshot_failure(format!(
      "{} comparison failed:\n\n  Expected result should be different from the actual one.",
      kind_of(path)
    )))
  }
}

/// The value matcher's decision, recorded on `report` as it goes. The
/// outer error is one that leaves nothing to report (an unreadable
/// baseline); the inner result is the matcher's.
fn judge_value(
  report: &mut Report<'_>,
  received: &SnapshotValue,
  options: &ImageComparatorOptions,
  mode: crate::config::UpdateSnapshotsMode,
) -> Result<Result<Option<String>, TestFailure>, TestFailure> {
  use crate::config::UpdateSnapshotsMode;
  let path = report.files.baseline.clone();
  if !path.exists() {
    let result = handle_missing(&path, received.bytes(), mode);
    report.missing(received.bytes(), &result);
    return Ok(result);
  }
  let expected = read_baseline(&path)?;
  if mode == UpdateSnapshotsMode::All {
    if received.bytes() == expected.as_slice() {
      return Ok(Ok(None));
    }
    let (difference, measure) = compare_value(&path, received, &expected, options)?;
    write_baseline(&path, received.bytes())?;
    tracing::info!(target: "ferridriver::snapshot", "{} is not the same, writing actual.", path.display());
    record_update(
      report,
      &expected,
      received.bytes(),
      difference.and_then(|d| d.diff).as_deref(),
      measure.as_ref(),
    );
    return Ok(Ok(None));
  }
  let (difference, measure) = compare_value(&path, received, &expected, options)?;
  report.measured(measure.as_ref());
  let Some(difference) = difference else {
    return Ok(Ok(None));
  };
  if mode == UpdateSnapshotsMode::Changed {
    write_baseline(&path, received.bytes())?;
    tracing::info!(target: "ferridriver::snapshot", "{} does not match, writing actual.", path.display());
    record_update(
      report,
      &expected,
      received.bytes(),
      difference.diff.as_deref(),
      measure.as_ref(),
    );
    return Ok(Ok(None));
  }
  report.status(SnapshotStatus::Changed, true, Some(difference.message.clone()));
  report.keep(Evidence::Expected, &expected);
  report.keep(Evidence::Actual, received.bytes());
  if let Some(diff) = &difference.diff {
    report.keep(Evidence::Diff, diff);
  }
  let mut message = format!("{} comparison failed:\n\n", kind_of(&path));
  for line in difference.message.lines() {
    message.push_str("  ");
    message.push_str(line);
    message.push('\n');
  }
  let _ = write!(
    message,
    "\nExpected: {}\nReceived: {}",
    path.display(),
    report.files.actual.display()
  );
  if difference.diff.is_some() {
    let _ = write!(message, "\n    Diff: {}", report.files.diff.display());
  }
  Ok(Err(TestFailure {
    message,
    stack: None,
    diff: (difference.diff.is_none()).then_some(difference.message),
    screenshot: difference.diff,
    error_context: None,
  }))
}

/// A baseline rewritten by an update mode: what it was, what replaced it
/// and where they differ, so the update can be reviewed.
pub(super) fn record_update(
  report: &mut Report<'_>,
  previous_baseline: &[u8],
  actual: &[u8],
  diff: Option<&[u8]>,
  measure: Option<&ImageComparison>,
) {
  report.status(SnapshotStatus::Updated, false, None);
  report.measured(measure);
  report.keep(Evidence::Expected, previous_baseline);
  report.keep(Evidence::Actual, actual);
  if let Some(diff) = diff {
    report.keep(Evidence::Diff, diff);
  }
}

/// `SnapshotHelper.kind`: images are screenshots, anything else a snapshot.
pub(super) fn kind_of(path: &Path) -> &'static str {
  if ImageFormat::from_path(path).is_some() {
    "Screenshot"
  } else {
    "Snapshot"
  }
}

pub(super) fn read_baseline(path: &Path) -> Result<Vec<u8>, TestFailure> {
  std::fs::read(path).map_err(|e| snapshot_failure(format!("read snapshot {}: {e}", path.display())))
}

/// The template `toMatchAriaSnapshot` generates a baseline against: it
/// can never match, so the poll runs to its end and the last live tree
/// becomes the baseline.
const IMPOSSIBLE_ARIA_MATCH: &str = "- none \"Generating new baseline\"";

/// `toMatchAriaSnapshot({ name? })`: the template lives in a file, which
/// the update modes write (`matchers/toMatchAriaSnapshot.ts`).
///
/// `matcher` judges one template against the live tree; it is polled
/// until it agrees or `timeout` passes. `Ok(Some(message))` is a pass
/// that must still fail the test softly (a baseline written under
/// `updateSnapshots: 'default'`).
///
/// # Errors
///
/// The mismatch, or a `.not` with no baseline to compare against.
pub async fn match_aria_file<F, Fut>(
  test_info: &TestInfo,
  name: &crate::snapshot_path::SnapshotName,
  is_not: bool,
  timeout: std::time::Duration,
  mut matcher: F,
) -> Result<Option<String>, TestFailure>
where
  F: FnMut(String) -> Fut,
  Fut: std::future::Future<Output = ferridriver::Result<ferridriver::locator::AriaSnapshotMatch>>,
{
  use crate::config::UpdateSnapshotsMode;
  use crate::snapshot_path::SnapshotKind;
  let mode = test_info.update_snapshots;
  if test_info.ignore_snapshots {
    return Ok(None);
  }
  // 1.51 moved the default extension to `.aria.yml`; a `.yml` baseline
  // written before still counts.
  let legacy = test_info
    .resolve_snapshot_paths_with_extension(SnapshotKind::Aria, name, false, Some(".yml"))
    .absolute_snapshot_path;
  let mut path = test_info
    .resolve_snapshot_paths(SnapshotKind::Aria, name, true)
    .absolute_snapshot_path;
  if !path.exists() && legacy.exists() {
    path = legacy;
  }
  let stored = std::fs::read_to_string(&path).unwrap_or_default();
  let missing = matches!(mode, UpdateSnapshotsMode::Missing | UpdateSnapshotsMode::Default) && stored.is_empty();
  if missing && is_not {
    return Err(snapshot_failure(
      "Matchers using \".not\" can't generate new baselines".to_string(),
    ));
  }
  let generate = !is_not && (mode == UpdateSnapshotsMode::All || missing);
  let template = if generate {
    IMPOSSIBLE_ARIA_MATCH.to_string()
  } else {
    stored
  };
  let template =
    crate::expect::aria_template_with_children(&template, test_info.expect.to_match_aria_snapshot.children.as_deref());

  let deadline = std::time::Instant::now() + timeout;
  let mut attempt = 0usize;
  let last = loop {
    let result = matcher(template.clone()).await;
    let settled = match &result {
      Ok(found) => found.matches != is_not,
      Err(_) => false,
    };
    let wait = ferridriver_expect::POLL_INTERVALS
      .get(attempt)
      .or_else(|| ferridriver_expect::POLL_INTERVALS.last())
      .copied()
      .unwrap_or(1000);
    let next = std::time::Instant::now() + std::time::Duration::from_millis(wait);
    if settled || next >= deadline {
      break result;
    }
    attempt += 1;
    tokio::time::sleep_until(tokio::time::Instant::from_std(next)).await;
  };
  let found = last.map_err(|e| snapshot_failure(format!("toMatchAriaSnapshot: {e}")))?;
  let pass = found.matches;

  if !is_not && (generate || (mode == UpdateSnapshotsMode::Changed && !pass)) {
    write_baseline(&path, found.regex.as_bytes())?;
    let relative = std::env::current_dir()
      .ok()
      .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf))
      .unwrap_or_else(|| path.clone());
    if missing {
      let message = format!("A snapshot doesn't exist at {}, writing actual.", relative.display());
      if mode == UpdateSnapshotsMode::Default {
        return Ok(Some(message));
      }
      tracing::info!(target: "ferridriver::snapshot", "{message}");
      return Ok(None);
    }
    tracing::info!(target: "ferridriver::snapshot", "A snapshot is generated at {}.", relative.display());
    return Ok(None);
  }
  if pass == is_not {
    return Err(snapshot_failure(if is_not {
      format!("Expected: not\n{template}\nReceived:\n{}", found.received)
    } else {
      let diff = similar::TextDiff::from_lines(&template, &found.received)
        .unified_diff()
        .header("Expected", "Received")
        .to_string();
      format!("toMatchAriaSnapshot failed:\n\n{diff}")
    }));
  }
  Ok(None)
}

/// `toMatchSnapshot.ts::handleMissing`: what a baseline that does not
/// exist means under `mode`. `Ok(Some(message))` asks the caller to fail
/// the test softly after the matcher passed.
pub(super) fn handle_missing(
  baseline: &Path,
  actual: &[u8],
  mode: crate::config::UpdateSnapshotsMode,
) -> Result<Option<String>, TestFailure> {
  use crate::config::UpdateSnapshotsMode;
  if mode == UpdateSnapshotsMode::None {
    return Err(snapshot_failure(format!(
      "A snapshot doesn't exist at {}.",
      baseline.display()
    )));
  }
  write_baseline(baseline, actual)?;
  let message = format!("A snapshot doesn't exist at {}, writing actual.", baseline.display());
  if mode == UpdateSnapshotsMode::Default {
    return Ok(Some(message));
  }
  tracing::info!(target: "ferridriver::snapshot", "{message}");
  Ok(None)
}

pub(super) fn write_baseline(baseline: &Path, actual: &[u8]) -> Result<(), TestFailure> {
  if let Some(parent) = baseline.parent() {
    std::fs::create_dir_all(parent).map_err(|e| snapshot_failure(format!("failed to create snapshot dir: {e}")))?;
  }
  std::fs::write(baseline, actual)
    .map_err(|e| snapshot_failure(format!("failed to write snapshot {}: {e}", baseline.display())))
}

pub(super) fn snapshot_failure(message: String) -> TestFailure {
  TestFailure {
    message,
    stack: None,
    diff: None,
    screenshot: None,
    error_context: None,
  }
}

mod screenshot;
#[cfg(test)]
mod tests;

pub use screenshot::{
  ScreenshotFiles, compare_screenshot_png, compare_screenshot_png_at, compare_screenshot_png_with, env_update_mode,
  judge_screenshot, settle_outside_host, to_have_screenshot,
};
