//! Snapshot assertions: `toMatchSnapshot` on a value and the baseline
//! side of `toHaveScreenshot`, with Playwright's update modes
//! (`matchers/toMatchSnapshot.ts`).

use std::path::{Path, PathBuf};

use crate::comparator::{ImageComparatorOptions, ImageFormat, compare_images};
use crate::model::{TestFailure, TestInfo};

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

/// Why a value does not match its baseline, and the diff image when the
/// baseline is an image.
struct Difference {
  message: String,
  diff: Option<Vec<u8>>,
}

/// `getComparator(mimeType)` applied to one value and its baseline.
fn compare_value(
  path: &Path,
  received: &SnapshotValue,
  expected: &[u8],
  options: &ImageComparatorOptions,
) -> Result<Option<Difference>, TestFailure> {
  if let Some(format) = ImageFormat::from_path(path) {
    let SnapshotValue::Bytes(actual) = received else {
      return Ok(Some(Difference {
        message: "Actual result should be a Buffer.".to_string(),
        diff: None,
      }));
    };
    return compare_images(format, actual, expected, options)
      .map(|mismatch| {
        mismatch.map(|m| Difference {
          message: m.message,
          diff: Some(m.diff),
        })
      })
      .map_err(snapshot_failure);
  }
  let is_text = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"));
  match received {
    SnapshotValue::Text(actual) => Ok(compare_text(actual, &String::from_utf8_lossy(expected))),
    SnapshotValue::Bytes(_) if is_text => Ok(Some(Difference {
      message: "Actual result should be a string".to_string(),
      diff: None,
    })),
    SnapshotValue::Bytes(actual) => Ok((actual.as_slice() != expected).then(|| Difference {
      message: "Buffers differ".to_string(),
      diff: None,
    })),
  }
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
/// `Ok(Some(message))` is a pass that must still fail the test softly:
/// the baseline was missing under `updateSnapshots: 'default'` and has
/// just been written.
///
/// # Errors
///
/// The comparison failure, with the actual value and any diff written
/// into the test's output directory.
pub fn match_value_snapshot(
  test_info: &TestInfo,
  received: &SnapshotValue,
  name: &crate::snapshot_path::SnapshotName,
  options: &ImageComparatorOptions,
  is_not: bool,
) -> Result<Option<String>, TestFailure> {
  use crate::config::UpdateSnapshotsMode;
  if test_info.ignore_snapshots {
    return Ok(None);
  }
  let resolved = test_info.resolve_snapshot_paths_with_extension(
    crate::snapshot_path::SnapshotKind::Snapshot,
    name,
    true,
    Some(received.anonymous_extension()),
  );
  let path = resolved.absolute_snapshot_path;
  let output = test_info.output_dir.join(&resolved.relative_output_path);
  let mode = test_info.update_snapshots;

  if is_not {
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
    let expected = read_baseline(&path)?;
    return if compare_value(&path, received, &expected, options)?.is_some() {
      Ok(None)
    } else {
      Err(snapshot_failure(format!(
        "{} comparison failed:\n\n  Expected result should be different from the actual one.",
        kind_of(&path)
      )))
    };
  }

  if !path.exists() {
    return handle_missing(&path, received.bytes(), mode);
  }
  let expected = read_baseline(&path)?;
  if mode == UpdateSnapshotsMode::All {
    if received.bytes() != expected.as_slice() {
      write_baseline(&path, received.bytes())?;
      tracing::info!(target: "ferridriver::snapshot", "{} is not the same, writing actual.", path.display());
    }
    return Ok(None);
  }
  let Some(difference) = compare_value(&path, received, &expected, options)? else {
    return Ok(None);
  };
  if mode == UpdateSnapshotsMode::Changed {
    write_baseline(&path, received.bytes())?;
    tracing::info!(target: "ferridriver::snapshot", "{} does not match, writing actual.", path.display());
    return Ok(None);
  }
  let actual_path = PathBuf::from(crate::snapshot_path::add_suffix_to_file_path(
    &output.to_string_lossy(),
    "-actual",
  ));
  let expected_path = PathBuf::from(crate::snapshot_path::add_suffix_to_file_path(
    &output.to_string_lossy(),
    "-expected",
  ));
  let diff_path = PathBuf::from(crate::snapshot_path::add_suffix_to_file_path(
    &output.to_string_lossy(),
    "-diff",
  ));
  if let Some(parent) = actual_path.parent() {
    let _ = std::fs::create_dir_all(parent);
  }
  let _ = std::fs::write(&actual_path, received.bytes());
  let _ = std::fs::write(&expected_path, &expected);
  let mut message = format!("{} comparison failed:\n\n", kind_of(&path));
  for line in difference.message.lines() {
    message.push_str("  ");
    message.push_str(line);
    message.push('\n');
  }
  message.push_str(&format!(
    "\nExpected: {}\nReceived: {}",
    path.display(),
    actual_path.display()
  ));
  if let Some(diff) = &difference.diff {
    let _ = std::fs::write(&diff_path, diff);
    message.push_str(&format!("\n    Diff: {}", diff_path.display()));
  }
  Err(TestFailure {
    message,
    stack: None,
    diff: (difference.diff.is_none()).then_some(difference.message),
    screenshot: difference.diff,
  })
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

/// The three files a screenshot comparison touches.
///
/// Playwright keeps the baseline where the template says and writes the
/// `-actual` / `-diff` copies into the OUTPUT directory, so a failing
/// run never leaves artifacts in the committed snapshot tree
/// (`matchers/toMatchSnapshot.ts:104-118`).
#[derive(Debug, Clone)]
pub struct ScreenshotFiles {
  pub baseline: PathBuf,
  pub actual: PathBuf,
  pub diff: PathBuf,
  /// The capture before the last, when two in a row never agreed.
  pub previous: PathBuf,
}

impl ScreenshotFiles {
  /// Baseline at `path`, with the actual/diff copies under
  /// `output_base`'s directory — the runner path.
  #[must_use]
  pub fn new(baseline: PathBuf, output_base: &Path) -> Self {
    Self {
      actual: PathBuf::from(crate::snapshot_path::add_suffix_to_file_path(
        &output_base.to_string_lossy(),
        "-actual",
      )),
      diff: PathBuf::from(crate::snapshot_path::add_suffix_to_file_path(
        &output_base.to_string_lossy(),
        "-diff",
      )),
      previous: PathBuf::from(crate::snapshot_path::add_suffix_to_file_path(
        &output_base.to_string_lossy(),
        "-previous",
      )),
      baseline,
    }
  }

  /// Everything beside the baseline — the `SNAPSHOT_DIR` path, which has
  /// no output directory to write into.
  #[must_use]
  pub fn beside(snapshot_dir: &Path, name: &str) -> Self {
    Self {
      baseline: snapshot_dir.join(format!("{name}.png")),
      actual: snapshot_dir.join(format!("{name}-actual.png")),
      diff: snapshot_dir.join(format!("{name}-diff.png")),
      previous: snapshot_dir.join(format!("{name}-previous.png")),
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
  let mut equal =
    |actual: &[u8], expectation: Option<&[u8]>, previous: Option<&Vec<u8>>| -> Result<bool, TestFailure> {
      let Some(expectation) = expectation else {
        return Ok(false);
      };
      let mismatch = compare_images(format, actual, expectation, options).map_err(snapshot_failure)?;
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
    let expectation = if first && expected.is_some() {
      expected
    } else {
      previous.as_deref()
    };
    if equal(current, expectation, previous.as_ref())? {
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
    });
  }
  let current = actual.clone().unwrap_or_default();
  if equal(&current, expected, None)? {
    return Ok(ScreenshotOutcome {
      actual: None,
      previous: None,
      error: None,
      diff: None,
    });
  }
  let (error, diff, previous) = intermediate.map_or((String::new(), None, None), |(m, d, p)| (m, Some(d), p));
  Ok(ScreenshotOutcome {
    actual,
    previous,
    error: Some(error),
    diff,
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
  use crate::config::UpdateSnapshotsMode;
  if options.ignore {
    return Ok(None);
  }
  let baseline = &paths.baseline;
  let has_snapshot = baseline.exists();
  if is_not && !has_snapshot {
    let tail = if mode == UpdateSnapshotsMode::None {
      "."
    } else {
      ", matchers using \".not\" won't write them automatically."
    };
    return Err(snapshot_failure(format!(
      "A snapshot doesn't exist at {}{tail}",
      baseline.display()
    )));
  }
  if !is_not && mode == UpdateSnapshotsMode::None && !has_snapshot {
    return Err(snapshot_failure(format!(
      "A snapshot doesn't exist at {}.",
      baseline.display()
    )));
  }
  let format = ImageFormat::from_path(baseline).unwrap_or(ImageFormat::Png);
  let comparator = ImageComparatorOptions {
    threshold: options.threshold,
    max_diff_pixels: options.max_diff_pixels,
    max_diff_pixel_ratio: options.max_diff_pixel_ratio,
    comparator: options.comparator,
  };

  if is_not {
    let expected = read_baseline(baseline)?;
    let outcome = expect_screenshot(format, Some(&expected), true, &comparator, timeout, capture).await?;
    return if outcome.error.is_none() {
      Ok(None)
    } else {
      Err(snapshot_failure(
        "Screenshot comparison failed:\n\n  Expected result should be different from the actual one.".to_string(),
      ))
    };
  }

  if !has_snapshot {
    let outcome = expect_screenshot(format, None, false, &comparator, timeout, capture).await?;
    return match (outcome.error, outcome.actual) {
      (None, Some(actual)) => handle_missing(baseline, &actual, mode),
      (error, _) => Err(screenshot_difference(
        paths,
        &error.unwrap_or_default(),
        outcome.previous.as_deref(),
        outcome.diff.as_deref(),
        None,
      )),
    };
  }

  let expected = read_baseline(baseline)?;
  let given = (mode != UpdateSnapshotsMode::All).then_some(expected.as_slice());
  let outcome = expect_screenshot(format, given, false, &comparator, timeout, capture).await?;
  match outcome.error {
    None => {
      if mode == UpdateSnapshotsMode::All
        && let Some(actual) = outcome.actual
        && compare_images(format, &actual, &expected, &comparator)
          .map_err(snapshot_failure)?
          .is_some()
      {
        write_baseline(baseline, &actual)?;
        tracing::info!(target: "ferridriver::snapshot", "{} is re-generated, writing actual.", baseline.display());
      }
      Ok(None)
    },
    Some(_) if matches!(mode, UpdateSnapshotsMode::All | UpdateSnapshotsMode::Changed) && outcome.actual.is_some() => {
      write_baseline(baseline, outcome.actual.as_deref().unwrap_or_default())?;
      tracing::info!(target: "ferridriver::snapshot", "{} is re-generated, writing actual.", baseline.display());
      Ok(None)
    },
    Some(error) => Err(screenshot_difference(
      paths,
      &error,
      outcome.previous.as_deref(),
      outcome.diff.as_deref(),
      outcome.actual.as_deref(),
    )),
  }
}

/// `handleDifferent` for a screenshot: the failure, with the images that
/// explain it written beside the run's output.
fn screenshot_difference(
  paths: &ScreenshotFiles,
  error: &str,
  previous: Option<&[u8]>,
  diff: Option<&[u8]>,
  actual: Option<&[u8]>,
) -> TestFailure {
  if let Some(parent) = paths.actual.parent() {
    let _ = std::fs::create_dir_all(parent);
  }
  let mut lines = vec![
    format!("Screenshot comparison failed:\n\n  {error}"),
    format!(
      "\n  Snapshot: {}\n",
      paths
        .baseline
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default()
    ),
    format!("Expected: {}", paths.baseline.display()),
  ];
  if let Some(actual) = actual {
    let _ = std::fs::write(&paths.actual, actual);
    lines.push(format!("Received: {}", paths.actual.display()));
  }
  if let Some(previous) = previous {
    let _ = std::fs::write(&paths.previous, previous);
    lines.push(format!("Previous: {}", paths.previous.display()));
  }
  if let Some(diff) = diff {
    let _ = std::fs::write(&paths.diff, diff);
    lines.push(format!("    Diff: {}", paths.diff.display()));
  }
  TestFailure {
    message: lines.join("\n"),
    stack: None,
    diff: None,
    screenshot: diff.map(<[u8]>::to_vec),
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
  })
}
