//! Screenshot baseline comparison and stability-loop polling for `toHaveScreenshot`.

use std::path::{Path, PathBuf};

use crate::comparator::{ImageComparatorOptions, ImageComparison, ImageFormat, measure_images};
use crate::model::TestFailure;
use crate::snapshot_record::{SnapshotComparisonKind, SnapshotStatus};

use super::{Evidence, Judged, Report, handle_missing, read_baseline, record_update, snapshot_failure, write_baseline};

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
      let failure = ferridriver_expect::AssertionFailure::new(message.clone(), None).with_soft(true);
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
  std::env::var("SNAPSHOT_DIR").map_or_else(|_| PathBuf::from("__snapshots__"), PathBuf::from)
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

/// The outcome of a screenshot expectation that ran out of time: the last
/// mismatch, or no stable pair of shots, or no capture at all.
fn timed_out_screenshot(
  intermediate: Option<(String, Vec<u8>, Option<Vec<u8>>)>,
  actual: Option<Vec<u8>>,
  measure: Option<ImageComparison>,
  timeout: std::time::Duration,
) -> ScreenshotOutcome {
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
  ScreenshotOutcome {
    actual,
    previous,
    error: Some(error),
    diff,
    measure,
  }
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
      return Ok(timed_out_screenshot(intermediate, actual, measure, timeout));
    }
    if wait > 0 {
      tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    }
    let previous = actual.take();
    // Only bound retry captures after the first baseline comparison has
    // already failed: cutting off an initial or baseline-writing capture
    // mid-flight turns CI GPU contention into a false timeout, while bounding
    // retries honours a short `{ timeout }` even when captures queue.
    let captured = if !first && expected.is_some() {
      let remaining = deadline.saturating_duration_since(std::time::Instant::now());
      let Ok(result) = tokio::time::timeout(remaining, capture()).await else {
        return Ok(timed_out_screenshot(intermediate, previous, measure, timeout));
      };
      result
    } else {
      capture().await
    };
    actual = match captured {
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
