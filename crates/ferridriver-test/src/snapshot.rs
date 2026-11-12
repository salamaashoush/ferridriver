//! Snapshot assertions: `toMatchSnapshot` on a value and the baseline
//! side of `toHaveScreenshot`, with Playwright's update modes
//! (`matchers/toMatchSnapshot.ts`).

use std::path::{Path, PathBuf};

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
fn mime_type(path: &Path) -> &'static str {
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
enum Evidence {
  Expected,
  Previous,
  Actual,
  Diff,
}

/// The record of one comparison as it is being decided, and the files
/// written beside the test's output to explain it.
struct Report<'a> {
  files: &'a ScreenshotFiles,
  record: SnapshotComparison,
  attachments: Vec<Attachment>,
}

impl<'a> Report<'a> {
  fn new(files: &'a ScreenshotFiles, kind: SnapshotComparisonKind, options: &ImageComparatorOptions) -> Self {
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

  fn status(&mut self, status: SnapshotStatus, failed: bool, message: Option<String>) {
    self.record.status = status;
    self.record.failed = failed;
    self.record.message = message;
  }

  fn measured(&mut self, comparison: Option<&ImageComparison>) {
    if let Some(comparison) = comparison {
      self.record.measured(comparison);
    }
  }

  /// Writes `bytes` to the evidence file and attaches it under
  /// Playwright's name for it. A file that cannot be written is left out
  /// rather than attached pointing nowhere.
  fn keep(&mut self, evidence: Evidence, bytes: &[u8]) {
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
  fn missing(&mut self, actual: &[u8], result: &Result<Option<String>, TestFailure>) {
    match result {
      Ok(soft) => {
        self.status(SnapshotStatus::Written, soft.is_some(), soft.clone());
        self.keep(Evidence::Expected, actual);
      },
      Err(failure) => self.status(SnapshotStatus::Missing, true, Some(failure.message.clone())),
    }
    self.keep(Evidence::Actual, actual);
  }

  fn finish(self, result: Result<Option<String>, TestFailure>) -> Judged {
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
  message.push_str(&format!(
    "\nExpected: {}\nReceived: {}",
    path.display(),
    report.files.actual.display()
  ));
  if difference.diff.is_some() {
    message.push_str(&format!("\n    Diff: {}", report.files.diff.display()));
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
fn record_update(
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
fn kind_of(path: &Path) -> &'static str {
  if ImageFormat::from_path(path).is_some() {
    "Screenshot"
  } else {
    "Snapshot"
  }
}

fn read_baseline(path: &Path) -> Result<Vec<u8>, TestFailure> {
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
fn handle_missing(
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

fn write_baseline(baseline: &Path, actual: &[u8]) -> Result<(), TestFailure> {
  if let Some(parent) = baseline.parent() {
    std::fs::create_dir_all(parent).map_err(|e| snapshot_failure(format!("failed to create snapshot dir: {e}")))?;
  }
  std::fs::write(baseline, actual)
    .map_err(|e| snapshot_failure(format!("failed to write snapshot {}: {e}", baseline.display())))
}

fn snapshot_failure(message: String) -> TestFailure {
  TestFailure {
    message,
    stack: None,
    diff: None,
    screenshot: None,
    error_context: None,
  }
}

/// The update mode outside a runner: `UPDATE_SNAPSHOTS` rewrites every
/// baseline, otherwise Playwright's default applies.
#[must_use]
pub fn env_update_mode() -> crate::config::UpdateSnapshotsMode {
  if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
    crate::config::UpdateSnapshotsMode::All
  } else {
    crate::config::UpdateSnapshotsMode::Default
  }
}

/// Compare a PNG screenshot against a stored baseline using
/// environment-variable defaults. Equivalent to
/// [`compare_screenshot_png_with`] with an empty option bag.
///
/// # Errors
///
/// Returns `TestFailure` with diff details if screenshots don't match.
pub fn compare_screenshot_png(actual_png: &[u8], name: &str) -> Result<(), TestFailure> {
  compare_screenshot_png_with(actual_png, name, &crate::expect::ScreenshotMatcherOptions::default())
}

/// Compare a PNG screenshot against a stored baseline.
///
/// Judged by [`crate::comparator::compare_images`]: `threshold`,
/// `max_diff_pixels`, `max_diff_pixel_ratio` and `comparator` apply.
///
/// # Errors
///
/// Returns `TestFailure` with diff details if the screenshots differ
/// beyond the configured budget.
pub fn compare_screenshot_png_with(
  actual_png: &[u8],
  name: &str,
  options: &crate::expect::ScreenshotMatcherOptions,
) -> Result<(), TestFailure> {
  settle_outside_host(compare_screenshot_png_at(
    &ScreenshotFiles::beside(&env_snapshot_dir(), name),
    actual_png,
    options,
    env_update_mode(),
  ))
}

/// Settle a comparison for a caller with no test host: a soft failure
/// goes to the current test's sink, and fails outright when there is no
/// test to record it in.
///
/// # Errors
///
/// The comparison's own failure, or the soft one when nothing takes it.
pub fn settle_outside_host(result: Result<Option<String>, TestFailure>) -> Result<(), TestFailure> {
  match result? {
    None => Ok(()),
    Some(message) => {
      let failure = ferridriver_expect::AssertionFailure::new(message.clone(), None).as_soft();
      if ferridriver_expect::soft::record_non_retriable(&failure) {
        Ok(())
      } else {
        Err(snapshot_failure(message))
      }
    },
  }
}

/// The files a screenshot comparison touches.
///
/// Playwright keeps the baseline where the template says and writes the
/// `-expected` / `-actual` / `-diff` copies into the OUTPUT directory, so
/// a failing run never leaves artifacts in the committed snapshot tree
/// (`matchers/toMatchSnapshot.ts:104-118`).
#[derive(Debug, Clone)]
pub struct ScreenshotFiles {
  pub baseline: PathBuf,
  /// The baseline's bytes as the comparison saw them, copied beside the
  /// output, so the attachment survives an update rewriting the baseline.
  pub expected: PathBuf,
  pub actual: PathBuf,
  pub diff: PathBuf,
  /// The capture before the last, when two in a row never agreed.
  pub previous: PathBuf,
  /// What the attachments are named after: the snapshot's path relative
  /// to the test's output directory (`hero.png`).
  pub name: String,
}

impl ScreenshotFiles {
  /// Baseline at `path`, with the evidence copies under `output_base`'s
  /// directory — the runner path.
  #[must_use]
  pub fn new(baseline: PathBuf, output_base: &Path, name: String) -> Self {
    let beside = |suffix: &str| {
      PathBuf::from(crate::snapshot_path::add_suffix_to_file_path(
        &output_base.to_string_lossy(),
        suffix,
      ))
    };
    Self {
      expected: beside("-expected"),
      actual: beside("-actual"),
      diff: beside("-diff"),
      previous: beside("-previous"),
      baseline,
      name,
    }
  }

  /// Everything beside the baseline — the `SNAPSHOT_DIR` path, which has
  /// no output directory to write into.
  #[must_use]
  pub fn beside(snapshot_dir: &Path, name: &str) -> Self {
    Self {
      baseline: snapshot_dir.join(format!("{name}.png")),
      expected: snapshot_dir.join(format!("{name}-expected.png")),
      actual: snapshot_dir.join(format!("{name}-actual.png")),
      diff: snapshot_dir.join(format!("{name}-diff.png")),
      previous: snapshot_dir.join(format!("{name}-previous.png")),
      name: format!("{name}.png"),
    }
  }
}

/// Snapshot directory for the non-runner path (`expect(locator)` used
/// outside a test).
fn env_snapshot_dir() -> PathBuf {
  std::env::var("SNAPSHOT_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|_| PathBuf::from("__snapshots__"))
}

/// What one `toHaveScreenshot` capture loop ended with
/// (`Page.expectScreenshot`).
struct ScreenshotOutcome {
  actual: Option<Vec<u8>>,
  previous: Option<Vec<u8>>,
  /// Why it failed: the last comparison, or the timeout.
  error: Option<String>,
  diff: Option<Vec<u8>>,
  /// The last comparison against the baseline itself, diff image left out.
  measure: Option<ImageComparison>,
}

/// `page.ts::expectScreenshot`. With `expected`, the first capture is
/// judged against it, then the page is captured until two shots in a row
/// agree and that stable one is judged; without, two shots in a row have
/// to agree and the stable one is the result. `is_not` inverts the
/// judgement.
async fn expect_screenshot<F, Fut>(
  format: crate::comparator::ImageFormat,
  expected: Option<&[u8]>,
  is_not: bool,
  options: &ImageComparatorOptions,
  timeout: std::time::Duration,
  mut capture: F,
) -> Result<ScreenshotOutcome, TestFailure>
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  let deadline = std::time::Instant::now() + timeout;
  let mut intervals = [0u64, 100, 250, 500].into_iter();
  let mut intermediate: Option<(String, Vec<u8>, Option<Vec<u8>>)> = None;
  let mut measure: Option<ImageComparison> = None;
  let mut equal = |actual: &[u8],
                   expectation: Option<&[u8]>,
                   previous: Option<&Vec<u8>>,
                   against_baseline: bool|
   -> Result<bool, TestFailure> {
    let Some(expectation) = expectation else {
      return Ok(false);
    };
    let mut comparison = measure_images(format, actual, expectation, options).map_err(snapshot_failure)?;
    let mismatch = comparison.mismatch.take();
    if against_baseline {
      measure = Some(comparison);
    }
    if mismatch.is_some() == is_not {
      return Ok(true);
    }
    if let Some(m) = mismatch {
      intermediate = Some((m.message, m.diff, previous.cloned()));
    }
    Ok(false)
  };
  let mut actual: Option<Vec<u8>> = None;
  let mut first = true;
  loop {
    let wait = intervals.next().unwrap_or(1000);
    let now = std::time::Instant::now();
    if now >= deadline || (wait > 0 && now + std::time::Duration::from_millis(wait) >= deadline) {
      let unstable = intermediate.as_ref().is_some_and(|(_, _, previous)| previous.is_some());
      let (error, diff, previous) = match intermediate {
        Some((_, diff, previous)) if unstable => (
          "Failed to take two consecutive stable screenshots.".to_string(),
          Some(diff),
          previous,
        ),
        Some((message, diff, previous)) => (message, Some(diff), previous),
        None => (format!("Timeout {}ms exceeded.", timeout.as_millis()), None, None),
      };
      return Ok(ScreenshotOutcome {
        actual,
        previous,
        error: Some(error),
        diff,
        measure,
      });
    }
    if wait > 0 {
      tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    }
    let previous = actual.take();
    actual = match capture().await {
      Ok(bytes) => Some(bytes),
      // A closed page, or a capture the engine refuses outright, cannot
      // succeed on a retry.
      Err(failure) if failure.message.contains("closed") || failure.message.contains("unsupported operation") => {
        return Err(failure);
      },
      Err(_) => {
        actual = previous;
        continue;
      },
    };
    let current = actual.as_deref().unwrap_or_default();
    let against_baseline = first && expected.is_some();
    let expectation = if against_baseline {
      expected
    } else {
      previous.as_deref()
    };
    if equal(current, expectation, previous.as_ref(), against_baseline)? {
      break;
    }
    first = false;
  }
  if expected.is_none() || first {
    return Ok(ScreenshotOutcome {
      actual: if expected.is_none() { actual } else { None },
      previous: None,
      error: None,
      diff: None,
      measure,
    });
  }
  let current = actual.clone().unwrap_or_default();
  if equal(&current, expected, None, true)? {
    return Ok(ScreenshotOutcome {
      actual: None,
      previous: None,
      error: None,
      diff: None,
      measure,
    });
  }
  let (error, diff, previous) = intermediate.map_or((String::new(), None, None), |(m, d, p)| (m, Some(d), p));
  Ok(ScreenshotOutcome {
    actual,
    previous,
    error: Some(error),
    diff,
    measure,
  })
}

/// `toHaveScreenshot`'s baseline handling (`matchers/toMatchSnapshot.ts`):
/// update modes, `.not`, and the stable-capture loop.
///
/// `Ok(Some(message))` is a pass that must still fail the test softly:
/// the baseline was missing under `updateSnapshots: 'default'` and has
/// just been written.
///
/// # Errors
///
/// The comparison failure, with the actual, previous and diff images
/// written beside the run's output.
pub async fn to_have_screenshot<F, Fut>(
  paths: &ScreenshotFiles,
  options: &crate::expect::ScreenshotMatcherOptions,
  mode: crate::config::UpdateSnapshotsMode,
  is_not: bool,
  timeout: std::time::Duration,
  capture: F,
) -> Result<Option<String>, TestFailure>
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  judge_screenshot(paths, options, mode, is_not, timeout, capture)
    .await
    .result
}

/// [`to_have_screenshot`] with the comparison it made and the files it
/// left, for a caller that records them on the test.
pub async fn judge_screenshot<F, Fut>(
  paths: &ScreenshotFiles,
  options: &crate::expect::ScreenshotMatcherOptions,
  mode: crate::config::UpdateSnapshotsMode,
  is_not: bool,
  timeout: std::time::Duration,
  capture: F,
) -> Judged
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  // The captures are the assertion's, not steps of their own.
  Box::pin(ferridriver::trace::within_assertion(compare_screenshots(
    paths, options, mode, is_not, timeout, capture,
  )))
  .await
}

async fn compare_screenshots<F, Fut>(
  paths: &ScreenshotFiles,
  options: &crate::expect::ScreenshotMatcherOptions,
  mode: crate::config::UpdateSnapshotsMode,
  is_not: bool,
  timeout: std::time::Duration,
  capture: F,
) -> Judged
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  use crate::config::UpdateSnapshotsMode;
  if options.ignore {
    return Judged::bare(Ok(None));
  }
  let baseline = &paths.baseline;
  let has_snapshot = baseline.exists();
  let format = ImageFormat::from_path(baseline).unwrap_or(ImageFormat::Png);
  let comparator = ImageComparatorOptions {
    threshold: options.threshold,
    max_diff_pixels: options.max_diff_pixels,
    max_diff_pixel_ratio: options.max_diff_pixel_ratio,
    comparator: options.comparator,
  };
  if is_not {
    return Judged::bare(compare_negated(paths, format, &comparator, mode, timeout, capture).await);
  }
  let mut report = Report::new(paths, SnapshotComparisonKind::Screenshot, &comparator);
  if mode == UpdateSnapshotsMode::None && !has_snapshot {
    let message = format!("A snapshot doesn't exist at {}.", baseline.display());
    report.status(SnapshotStatus::Missing, true, Some(message.clone()));
    return report.finish(Err(snapshot_failure(message)));
  }
  let judged = if has_snapshot {
    judge_against_baseline(&mut report, format, &comparator, mode, timeout, capture).await
  } else {
    judge_without_baseline(&mut report, format, &comparator, mode, timeout, capture).await
  };
  match judged {
    Ok(result) => report.finish(result),
    Err(failure) => Judged::bare(Err(failure)),
  }
}

async fn compare_negated<F, Fut>(
  paths: &ScreenshotFiles,
  format: ImageFormat,
  comparator: &ImageComparatorOptions,
  mode: crate::config::UpdateSnapshotsMode,
  timeout: std::time::Duration,
  capture: F,
) -> Result<Option<String>, TestFailure>
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  use crate::config::UpdateSnapshotsMode;
  if !paths.baseline.exists() {
    let tail = if mode == UpdateSnapshotsMode::None {
      "."
    } else {
      ", matchers using \".not\" won't write them automatically."
    };
    return Err(snapshot_failure(format!(
      "A snapshot doesn't exist at {}{tail}",
      paths.baseline.display()
    )));
  }
  let expected = read_baseline(&paths.baseline)?;
  let outcome = expect_screenshot(format, Some(&expected), true, comparator, timeout, capture).await?;
  if outcome.error.is_none() {
    Ok(None)
  } else {
    Err(snapshot_failure(
      "Screenshot comparison failed:\n\n  Expected result should be different from the actual one.".to_string(),
    ))
  }
}

/// No baseline: two stable captures become it, or the instability is the
/// failure.
async fn judge_without_baseline<F, Fut>(
  report: &mut Report<'_>,
  format: ImageFormat,
  comparator: &ImageComparatorOptions,
  mode: crate::config::UpdateSnapshotsMode,
  timeout: std::time::Duration,
  capture: F,
) -> Result<Result<Option<String>, TestFailure>, TestFailure>
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  let outcome = expect_screenshot(format, None, false, comparator, timeout, capture).await?;
  match (outcome.error, outcome.actual) {
    (None, Some(actual)) => {
      let result = handle_missing(&report.files.baseline, &actual, mode);
      report.missing(&actual, &result);
      Ok(result)
    },
    (error, actual) => {
      let error = error.unwrap_or_default();
      report.status(SnapshotStatus::Missing, true, Some(error.clone()));
      let images = Images {
        expected: None,
        previous: outcome.previous.as_deref(),
        actual: actual.as_deref(),
        diff: outcome.diff.as_deref(),
      };
      Ok(Err(screenshot_difference(report, &error, &images)))
    },
  }
}

/// A baseline exists: judged as is, or rewritten under `all` and
/// `changed`, in which case the record still says what changed.
async fn judge_against_baseline<F, Fut>(
  report: &mut Report<'_>,
  format: ImageFormat,
  comparator: &ImageComparatorOptions,
  mode: crate::config::UpdateSnapshotsMode,
  timeout: std::time::Duration,
  capture: F,
) -> Result<Result<Option<String>, TestFailure>, TestFailure>
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  use crate::config::UpdateSnapshotsMode;
  let baseline = report.files.baseline.clone();
  let expected = read_baseline(&baseline)?;
  let given = (mode != UpdateSnapshotsMode::All).then_some(expected.as_slice());
  let outcome = expect_screenshot(format, given, false, comparator, timeout, capture).await?;
  let rewrites = matches!(mode, UpdateSnapshotsMode::All | UpdateSnapshotsMode::Changed);
  match (outcome.error, outcome.actual) {
    (None, Some(actual)) if mode == UpdateSnapshotsMode::All => {
      let mut measure = measure_images(format, &actual, &expected, comparator).map_err(snapshot_failure)?;
      let mismatch = measure.mismatch.take();
      report.measured(Some(&measure));
      if let Some(mismatch) = mismatch {
        rewrite(report, &expected, &actual, Some(&mismatch.diff), Some(&measure))?;
      }
      Ok(Ok(None))
    },
    (None, _) => {
      report.measured(outcome.measure.as_ref());
      Ok(Ok(None))
    },
    (Some(_), Some(actual)) if rewrites => {
      let mut measure = measure_images(format, &actual, &expected, comparator).map_err(snapshot_failure)?;
      let diff = measure.mismatch.take().map(|m| m.diff).or(outcome.diff);
      rewrite(report, &expected, &actual, diff.as_deref(), Some(&measure))?;
      Ok(Ok(None))
    },
    (Some(error), actual) => {
      report.status(SnapshotStatus::Changed, true, Some(error.clone()));
      let measured = actual
        .as_deref()
        .and_then(|actual| measure_images(format, actual, &expected, comparator).ok());
      report.measured(measured.as_ref().or(outcome.measure.as_ref()));
      let images = Images {
        expected: Some(&expected),
        previous: outcome.previous.as_deref(),
        actual: actual.as_deref(),
        diff: outcome.diff.as_deref(),
      };
      Ok(Err(screenshot_difference(report, &error, &images)))
    },
  }
}

/// Writes the stable capture over the baseline and records the update.
fn rewrite(
  report: &mut Report<'_>,
  expected: &[u8],
  actual: &[u8],
  diff: Option<&[u8]>,
  measure: Option<&ImageComparison>,
) -> Result<(), TestFailure> {
  write_baseline(&report.files.baseline, actual)?;
  tracing::info!(target: "ferridriver::snapshot", "{} is re-generated, writing actual.", report.files.baseline.display());
  record_update(report, expected, actual, diff, measure);
  Ok(())
}

/// The images a failed comparison has to show.
struct Images<'a> {
  expected: Option<&'a [u8]>,
  previous: Option<&'a [u8]>,
  actual: Option<&'a [u8]>,
  diff: Option<&'a [u8]>,
}

/// `handleDifferent` for a screenshot: the failure, with the images that
/// explain it written beside the run's output and attached.
fn screenshot_difference(report: &mut Report<'_>, error: &str, images: &Images<'_>) -> TestFailure {
  let files = report.files;
  let mut lines = vec![
    format!("Screenshot comparison failed:\n\n  {error}"),
    format!(
      "\n  Snapshot: {}\n",
      files
        .baseline
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default()
    ),
    format!("Expected: {}", files.baseline.display()),
  ];
  if let Some(expected) = images.expected {
    report.keep(Evidence::Expected, expected);
  }
  if let Some(previous) = images.previous {
    report.keep(Evidence::Previous, previous);
  }
  if let Some(actual) = images.actual {
    report.keep(Evidence::Actual, actual);
    lines.push(format!("Received: {}", files.actual.display()));
  }
  if images.previous.is_some() {
    lines.push(format!("Previous: {}", files.previous.display()));
  }
  if let Some(diff) = images.diff {
    report.keep(Evidence::Diff, diff);
    lines.push(format!("    Diff: {}", files.diff.display()));
  }
  TestFailure {
    message: lines.join("\n"),
    stack: None,
    diff: None,
    screenshot: images.diff.map(<[u8]>::to_vec),
    error_context: None,
  }
}

/// [`compare_screenshot_png_with`] with an explicit snapshot directory
/// and update flag — the test-runner path, where both come from the
/// live `TestInfo` instead of environment variables.
///
/// # Errors
///
/// Returns `TestFailure` with diff details if the screenshots differ
/// beyond the configured budget.
pub fn compare_screenshot_png_at(
  paths: &ScreenshotFiles,
  actual_png: &[u8],
  options: &crate::expect::ScreenshotMatcherOptions,
  mode: crate::config::UpdateSnapshotsMode,
) -> Result<Option<String>, TestFailure> {
  use crate::config::UpdateSnapshotsMode;
  // `--ignore-snapshots`: the matcher succeeds without ever touching
  // the baseline file.
  if options.ignore {
    return Ok(None);
  }
  if !paths.baseline.exists() {
    return handle_missing(&paths.baseline, actual_png, mode);
  }
  if mode == UpdateSnapshotsMode::All {
    write_baseline(&paths.baseline, actual_png)?;
    return Ok(None);
  }
  match compare_existing_png(paths, actual_png, options) {
    Err(_) if mode == UpdateSnapshotsMode::Changed => {
      write_baseline(&paths.baseline, actual_png)?;
      Ok(None)
    },
    other => other.map(|()| None),
  }
}

fn compare_existing_png(
  paths: &ScreenshotFiles,
  actual_png: &[u8],
  options: &crate::expect::ScreenshotMatcherOptions,
) -> Result<(), TestFailure> {
  let expected = std::fs::read(&paths.baseline)
    .map_err(|e| snapshot_failure(format!("read snapshot {}: {e}", paths.baseline.display())))?;
  if expected == actual_png {
    return Ok(());
  }
  let format =
    crate::comparator::ImageFormat::from_path(&paths.baseline).unwrap_or(crate::comparator::ImageFormat::Png);
  let comparison = crate::comparator::compare_images(
    format,
    actual_png,
    &expected,
    &crate::comparator::ImageComparatorOptions {
      threshold: options.threshold,
      max_diff_pixels: options.max_diff_pixels,
      max_diff_pixel_ratio: options.max_diff_pixel_ratio,
      comparator: options.comparator,
    },
  )
  .map_err(snapshot_failure)?;
  let Some(mismatch) = comparison else {
    return Ok(());
  };
  if let Some(parent) = paths.actual.parent() {
    let _ = std::fs::create_dir_all(parent);
  }
  let _ = std::fs::write(&paths.actual, actual_png);
  let _ = std::fs::write(&paths.diff, &mismatch.diff);
  let name = paths
    .baseline
    .file_name()
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_default();
  Err(TestFailure {
    message: format!(
      "Screenshot comparison failed:\n\n  {}\n\n  Snapshot: {name}\n\nExpected: {}\nReceived: {}\n    Diff: {}",
      mismatch.message,
      paths.baseline.display(),
      paths.actual.display(),
      paths.diff.display(),
    ),
    stack: None,
    diff: None,
    screenshot: Some(mismatch.diff),
    error_context: None,
  })
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::config::UpdateSnapshotsMode;
  use crate::expect::ScreenshotMatcherOptions;

  fn png(width: u32, height: u32, paint: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(paint(x, y)));
    let mut bytes = Vec::new();
    image
      .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
      .unwrap();
    bytes
  }

  fn solid(color: [u8; 4]) -> Vec<u8> {
    png(10, 10, |_, _| color)
  }

  const RED: [u8; 4] = [255, 0, 0, 255];
  const BLUE: [u8; 4] = [0, 0, 255, 255];

  fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
      "ferri-snapshot-{label}-{}-{}",
      std::process::id(),
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn files(dir: &Path) -> ScreenshotFiles {
    ScreenshotFiles::new(dir.join("base/hero.png"), &dir.join("out/hero.png"), "hero.png".into())
  }

  async fn judge(files: &ScreenshotFiles, mode: UpdateSnapshotsMode, shot: Vec<u8>) -> Judged {
    judge_screenshot(
      files,
      &ScreenshotMatcherOptions::default(),
      mode,
      false,
      Duration::from_secs(5),
      move || {
        let shot = shot.clone();
        async move { Ok(shot) }
      },
    )
    .await
  }

  fn names(judged: &Judged) -> Vec<&str> {
    judged.attachments.iter().map(|a| a.name.as_str()).collect()
  }

  fn attached_bytes(judged: &Judged, name: &str) -> Vec<u8> {
    let attachment = judged.attachments.iter().find(|a| a.name == name).unwrap();
    let AttachmentBody::Path(path) = &attachment.body else {
      panic!("{name} is not a file");
    };
    std::fs::read(path).unwrap()
  }

  #[tokio::test]
  async fn a_missing_baseline_is_written_and_attached() {
    let dir = scratch("written");
    let files = files(&dir);
    let judged = judge(&files, UpdateSnapshotsMode::Missing, solid(RED)).await;
    let record = judged.comparison.clone().unwrap();
    assert!(matches!(judged.result, Ok(None)));
    assert_eq!(record.status, SnapshotStatus::Written);
    assert!(!record.failed);
    assert_eq!(record.kind, SnapshotComparisonKind::Screenshot);
    assert_eq!(record.name, "hero.png");
    assert_eq!(record.actual.as_deref(), Some("hero-actual.png"));
    assert_eq!(names(&judged), ["hero-expected.png", "hero-actual.png"]);
    assert_eq!(std::fs::read(&files.baseline).unwrap(), solid(RED));
    assert_eq!(attached_bytes(&judged, "hero-actual.png"), solid(RED));
  }

  #[tokio::test]
  async fn the_default_mode_writes_the_baseline_and_fails_softly() {
    let dir = scratch("default");
    let judged = judge(&files(&dir), UpdateSnapshotsMode::Default, solid(RED)).await;
    let record = judged.comparison.unwrap();
    assert!(matches!(judged.result, Ok(Some(_))));
    assert_eq!(record.status, SnapshotStatus::Written);
    assert!(record.failed);
  }

  #[tokio::test]
  async fn none_reports_a_missing_baseline_without_writing_it() {
    let dir = scratch("none");
    let files = files(&dir);
    let judged = judge(&files, UpdateSnapshotsMode::None, solid(RED)).await;
    let record = judged.comparison.clone().unwrap();
    assert!(judged.result.is_err());
    assert_eq!(record.status, SnapshotStatus::Missing);
    assert!(record.failed);
    assert_eq!(names(&judged), Vec::<&str>::new());
    assert!(!files.baseline.exists());
  }

  #[tokio::test]
  async fn a_match_is_passed_with_its_measurements() {
    let dir = scratch("passed");
    let files = files(&dir);
    write_baseline(&files.baseline, &solid(RED)).unwrap();
    let judged = judge(&files, UpdateSnapshotsMode::Missing, solid(RED)).await;
    let record = judged.comparison.clone().unwrap();
    assert!(matches!(judged.result, Ok(None)));
    assert_eq!(record.status, SnapshotStatus::Passed);
    assert_eq!((record.diff_pixels, record.diff_ratio), (Some(0), Some(0.0)));
    assert_eq!((record.width, record.height), (Some(10), Some(10)));
    assert_eq!(names(&judged), Vec::<&str>::new());
  }

  #[tokio::test]
  async fn a_difference_fails_with_expected_actual_and_diff() {
    let dir = scratch("changed");
    let files = files(&dir);
    write_baseline(&files.baseline, &solid(RED)).unwrap();
    let judged = judge(&files, UpdateSnapshotsMode::Missing, solid(BLUE)).await;
    let record = judged.comparison.clone().unwrap();
    let failure = judged.result.as_ref().unwrap_err();
    assert!(
      failure.message.contains("Screenshot comparison failed"),
      "{}",
      failure.message
    );
    assert_eq!(record.status, SnapshotStatus::Changed);
    assert!(record.failed);
    assert_eq!(record.diff_pixels, Some(100));
    assert_eq!(record.diff_ratio, Some(1.0));
    assert_eq!(
      (
        record.expected.as_deref(),
        record.actual.as_deref(),
        record.diff.as_deref()
      ),
      (
        Some("hero-expected.png"),
        Some("hero-actual.png"),
        Some("hero-diff.png")
      )
    );
    assert_eq!(
      names(&judged),
      ["hero-expected.png", "hero-actual.png", "hero-diff.png"]
    );
    assert_eq!(attached_bytes(&judged, "hero-expected.png"), solid(RED));
    assert_eq!(attached_bytes(&judged, "hero-actual.png"), solid(BLUE));
    assert_eq!(
      std::fs::read(&files.baseline).unwrap(),
      solid(RED),
      "the baseline stays"
    );
  }

  #[tokio::test]
  async fn an_update_rewrites_the_baseline_and_keeps_what_it_replaced() {
    for mode in [UpdateSnapshotsMode::Changed, UpdateSnapshotsMode::All] {
      let dir = scratch("updated");
      let files = files(&dir);
      write_baseline(&files.baseline, &solid(RED)).unwrap();
      let judged = judge(&files, mode, solid(BLUE)).await;
      let record = judged.comparison.clone().unwrap();
      assert!(matches!(judged.result, Ok(None)), "{mode:?}");
      assert_eq!(record.status, SnapshotStatus::Updated, "{mode:?}");
      assert!(!record.failed);
      assert_eq!(record.diff_pixels, Some(100));
      assert_eq!(
        names(&judged),
        ["hero-expected.png", "hero-actual.png", "hero-diff.png"]
      );
      assert_eq!(
        attached_bytes(&judged, "hero-expected.png"),
        solid(RED),
        "the replaced baseline"
      );
      assert_eq!(std::fs::read(&files.baseline).unwrap(), solid(BLUE));
    }
  }

  #[tokio::test]
  async fn not_matchers_and_ignored_snapshots_record_nothing() {
    let dir = scratch("not");
    let files = files(&dir);
    write_baseline(&files.baseline, &solid(RED)).unwrap();
    let negated = judge_screenshot(
      &files,
      &ScreenshotMatcherOptions::default(),
      UpdateSnapshotsMode::Missing,
      true,
      Duration::from_secs(5),
      || async { Ok(solid(BLUE)) },
    )
    .await;
    assert!(negated.result.is_ok());
    assert!(negated.comparison.is_none());
    let ignored = judge_screenshot(
      &files,
      &ScreenshotMatcherOptions {
        ignore: true,
        ..ScreenshotMatcherOptions::default()
      },
      UpdateSnapshotsMode::Missing,
      false,
      Duration::from_secs(5),
      || async { Ok(solid(BLUE)) },
    )
    .await;
    assert!(ignored.comparison.is_none());
  }

  #[test]
  fn the_comparator_counts_differing_pixels_either_way() {
    let base = solid(RED);
    let quarter = png(10, 10, |x, y| if x < 5 && y < 5 { BLUE } else { RED });
    let within = ImageComparatorOptions {
      max_diff_pixel_ratio: Some(0.5),
      ..ImageComparatorOptions::default()
    };
    let passed = measure_images(ImageFormat::Png, &quarter, &base, &within).unwrap();
    assert!(passed.mismatch.is_none());
    assert_eq!((passed.diff_pixels, passed.total_pixels), (25, 100));
    assert!((passed.diff_ratio() - 0.25).abs() < f64::EPSILON);
    let failed = measure_images(ImageFormat::Png, &quarter, &base, &ImageComparatorOptions::default()).unwrap();
    assert!(failed.mismatch.is_some());
    assert_eq!(failed.diff_pixels, 25);
  }

  fn value_info(dir: &Path, mode: UpdateSnapshotsMode) -> TestInfo {
    let mut info = TestInfo::new_anonymous();
    info.output_dir = dir.join("out");
    info.snapshot_dir = dir.join("snaps");
    info.snapshot_path_template = Some(format!("{}/snaps/{{arg}}{{ext}}", dir.display()));
    info.update_snapshots = mode;
    info
  }

  #[test]
  fn value_snapshots_record_written_changed_and_updated() {
    let dir = scratch("value");
    let name = crate::snapshot_path::SnapshotName::One("value.txt".into());
    let options = ImageComparatorOptions::default();
    let text = |s: &str| SnapshotValue::Text(s.into());

    let written = match_value_snapshot(
      &value_info(&dir, UpdateSnapshotsMode::Missing),
      &text("one"),
      &name,
      &options,
      false,
    );
    let record = written.comparison.clone().unwrap();
    assert_eq!(
      (record.kind, record.status),
      (SnapshotComparisonKind::Snapshot, SnapshotStatus::Written)
    );
    assert_eq!(record.name, "value.txt");
    assert_eq!(names(&written), ["value-expected.txt", "value-actual.txt"]);
    assert_eq!(written.attachments[0].content_type, "text/plain");

    let passed = match_value_snapshot(
      &value_info(&dir, UpdateSnapshotsMode::None),
      &text("one"),
      &name,
      &options,
      false,
    );
    assert_eq!(passed.comparison.unwrap().status, SnapshotStatus::Passed);

    let changed = match_value_snapshot(
      &value_info(&dir, UpdateSnapshotsMode::None),
      &text("two"),
      &name,
      &options,
      false,
    );
    let record = changed.comparison.clone().unwrap();
    assert!(changed.result.is_err());
    assert_eq!(record.status, SnapshotStatus::Changed);
    assert_eq!(names(&changed), ["value-expected.txt", "value-actual.txt"]);

    let updated = match_value_snapshot(
      &value_info(&dir, UpdateSnapshotsMode::Changed),
      &text("two"),
      &name,
      &options,
      false,
    );
    assert_eq!(updated.comparison.as_ref().unwrap().status, SnapshotStatus::Updated);
    assert_eq!(attached_bytes(&updated, "value-expected.txt"), b"one");
  }
}
