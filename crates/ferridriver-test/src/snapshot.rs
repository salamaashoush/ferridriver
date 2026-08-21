//! Text snapshot testing: save expected output to `.snap` files, diff on mismatch.
//!
//! ```ignore
//! use ferridriver_test::snapshot::assert_snapshot;
//!
//! let info: Arc<TestInfo> = pool.get("test_info").await?;
//! assert_snapshot(&info, &page.content().await?, "page-content", false)?;
//! ```
//!
//! First run: creates the `.snap` file (test passes).
//! Subsequent: compares, fails with unified diff on mismatch.
//! With `update = true` (or `--update-snapshots`): overwrites the snap file.

use std::path::{Path, PathBuf};

use crate::model::{TestFailure, TestInfo};

/// Assert that `actual` matches the stored snapshot.
///
/// # Errors
///
/// Returns `TestFailure` with a unified diff if the snapshot doesn't match.
pub fn assert_snapshot(
  test_info: &TestInfo,
  actual: &str,
  name: &crate::snapshot_path::SnapshotName,
  update: bool,
) -> Result<(), TestFailure> {
  use crate::config::UpdateSnapshotsMode;

  // `--ignore-snapshots`: skip every comparison and write — the test still runs
  // but never fails on a snapshot mismatch.
  if test_info.ignore_snapshots && !update {
    return Ok(());
  }

  let resolved = test_info.resolve_snapshot_paths(crate::snapshot_path::SnapshotKind::Snapshot, name, true);
  let snap_path = resolved.absolute_snapshot_path;
  let name = snap_path
    .file_name()
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_default();
  let name = name.as_str();

  // Resolve effective update behavior from mode + legacy bool.
  let mode = test_info.update_snapshots;
  let should_create = update
    || matches!(
      mode,
      UpdateSnapshotsMode::All | UpdateSnapshotsMode::Missing | UpdateSnapshotsMode::Changed
    );
  let should_update = update || matches!(mode, UpdateSnapshotsMode::All | UpdateSnapshotsMode::Changed);

  if matches!(mode, UpdateSnapshotsMode::None) && !snap_path.exists() {
    return Err(TestFailure {
      message: format!("snapshot '{name}' missing and updateSnapshots is 'none'"),
      stack: None,
      diff: None,
      screenshot: None,
    });
  }

  if (should_update && snap_path.exists()) || (should_create && !snap_path.exists()) {
    if let Some(parent) = snap_path.parent() {
      std::fs::create_dir_all(parent).map_err(|e| TestFailure {
        message: format!("failed to create snapshot dir: {e}"),
        stack: None,
        diff: None,
        screenshot: None,
      })?;
    }
    std::fs::write(&snap_path, actual).map_err(|e| TestFailure {
      message: format!("failed to write snapshot: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?;
    return Ok(());
  }

  let expected = std::fs::read_to_string(&snap_path).map_err(|e| TestFailure {
    message: format!("failed to read snapshot '{}': {e}", snap_path.display()),
    stack: None,
    diff: None,
    screenshot: None,
  })?;

  if expected == actual {
    return Ok(());
  }

  // Generate unified diff.
  let diff = similar::TextDiff::from_lines(expected.as_str(), actual);
  let mut diff_str = String::new();
  for change in diff.iter_all_changes() {
    let sign = match change.tag() {
      similar::ChangeTag::Delete => "-",
      similar::ChangeTag::Insert => "+",
      similar::ChangeTag::Equal => " ",
    };
    diff_str.push_str(&format!("{sign}{change}"));
  }

  Err(TestFailure {
    message: format!(
      "snapshot '{name}' mismatch ({})\nRun with --update-snapshots to update.",
      snap_path.display()
    ),
    stack: None,
    diff: Some(diff_str),
    screenshot: None,
  })
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
/// Honoured option fields:
/// - `threshold` — per-channel pixel tolerance in `[0, 1]`. Mapped
///   to `0–255` for the byte-wise comparison. Falls back to the
///   `SCREENSHOT_THRESHOLD` env var (raw `0–255` units), then `2`.
/// - `max_diff_pixels` — accept up to N differing pixels even when
///   per-pixel deltas exceed the threshold.
/// - `max_diff_pixel_ratio` — fractional equivalent of the above
///   (`0.01` = 1% of total pixels).
///
/// `mask`, `mask_color`, `animations`, `caret`, `clip`, `scale`,
/// `style_path` are accepted on the option struct for parity but
/// not yet wired into the screenshot capture path.
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
  let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
  compare_screenshot_png_at(
    &ScreenshotFiles::beside(&env_snapshot_dir(), name),
    actual_png,
    options,
    update,
  )
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

/// Capture and compare until the baseline matches or the budget runs
/// out — Playwright's `toHaveScreenshot` retries rather than judging one
/// frame (`matchers/toMatchSnapshot.ts:352`, which races the capture
/// against `helper.options.timeout ?? this.timeout`).
///
/// `timeout` is the per-call `timeout`, else `expect.toHaveScreenshot`'s,
/// else the assertion's own — resolved by the caller. A missing baseline
/// is written on the first capture, so nothing retries there.
///
/// # Errors
///
/// The last comparison failure, once the deadline passes.
pub async fn screenshot_until_match<F, Fut>(
  paths: &ScreenshotFiles,
  options: &crate::expect::ScreenshotMatcherOptions,
  update: bool,
  timeout: std::time::Duration,
  mut capture: F,
) -> Result<(), TestFailure>
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Result<Vec<u8>, TestFailure>>,
{
  let deadline = std::time::Instant::now() + timeout;
  let mut attempt = 0usize;
  loop {
    let png = capture().await?;
    let result = compare_screenshot_png_at(paths, &png, options, update);
    let Err(failure) = result else { return Ok(()) };
    let wait = ferridriver_expect::POLL_INTERVALS
      .get(attempt)
      .or_else(|| ferridriver_expect::POLL_INTERVALS.last())
      .copied()
      .unwrap_or(1000);
    let next = std::time::Instant::now() + std::time::Duration::from_millis(wait);
    if next >= deadline {
      return Err(failure);
    }
    attempt += 1;
    tokio::time::sleep_until(tokio::time::Instant::from_std(next)).await;
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
  update: bool,
) -> Result<(), TestFailure> {
  let snap_path = paths.baseline.clone();
  let diff_path = paths.diff.clone();
  let actual_path = paths.actual.clone();
  let snap_dir = snap_path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
  let snap_dir = snap_dir.as_path();
  let name = snap_path
    .file_stem()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default();
  let name = name.as_str();
  // `--ignore-snapshots`: the matcher succeeds without ever touching
  // the baseline file. The text-snapshot path already short-circuits
  // here via `TestInfo::ignore_snapshots`; the screenshot path threads
  // the flag through `ScreenshotMatcherOptions::ignore` because the
  // matcher chain doesn't carry a TestInfo reference today.
  if options.ignore {
    return Ok(());
  }
  if update || !snap_path.exists() {
    if let Some(parent) = snap_path.parent() {
      std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&snap_path, actual_png).map_err(|e| TestFailure {
      message: format!("write screenshot: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?;
    return Ok(());
  }

  let expected_png = std::fs::read(&snap_path).map_err(|e| TestFailure {
    message: format!("read snapshot: {e}"),
    stack: None,
    diff: None,
    screenshot: None,
  })?;

  if expected_png == actual_png {
    return Ok(());
  }

  let expected_img = image::load_from_memory_with_format(&expected_png, image::ImageFormat::Png)
    .map_err(|e| TestFailure {
      message: format!("decode expected PNG: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?
    .to_rgba8();

  let actual_img = image::load_from_memory_with_format(actual_png, image::ImageFormat::Png)
    .map_err(|e| TestFailure {
      message: format!("decode actual PNG: {e}"),
      stack: None,
      diff: None,
      screenshot: None,
    })?
    .to_rgba8();

  let (ew, eh) = expected_img.dimensions();
  let (aw, ah) = actual_img.dimensions();

  if ew != aw || eh != ah {
    let _ = std::fs::create_dir_all(snap_dir);
    let _ = std::fs::write(&actual_path, actual_png);
    return Err(TestFailure {
      message: format!(
        "screenshot '{name}' size mismatch: expected {ew}x{eh}, got {aw}x{ah}\n\
         actual saved to: {}",
        actual_path.display()
      ),
      stack: None,
      diff: None,
      screenshot: Some(actual_png.to_vec()),
    });
  }

  // Threshold precedence: explicit option `threshold` (0..1 mapped to
  // 0..255) > SCREENSHOT_THRESHOLD env (raw 0..255) > default 2.
  let threshold: u8 = options
    .threshold
    .map(f64_threshold_to_u8)
    .or_else(|| std::env::var("SCREENSHOT_THRESHOLD").ok().and_then(|v| v.parse().ok()))
    .unwrap_or(2);

  let mut diff_img = image::RgbaImage::new(ew, eh);
  let mut mismatch_count: u64 = 0;
  let total_pixels = u64::from(ew) * u64::from(eh);

  let expected_pixels = expected_img.as_raw();
  let actual_pixels = actual_img.as_raw();

  for i in (0..expected_pixels.len()).step_by(4) {
    let dr = expected_pixels[i].abs_diff(actual_pixels[i]);
    let dg = expected_pixels[i + 1].abs_diff(actual_pixels[i + 1]);
    let db = expected_pixels[i + 2].abs_diff(actual_pixels[i + 2]);

    let pixel_idx = i / 4;
    let x = (pixel_idx % ew as usize) as u32;
    let y = (pixel_idx / ew as usize) as u32;

    if dr > threshold || dg > threshold || db > threshold {
      mismatch_count += 1;
      diff_img.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
    } else {
      diff_img.put_pixel(
        x,
        y,
        image::Rgba([
          actual_pixels[i] / 3,
          actual_pixels[i + 1] / 3,
          actual_pixels[i + 2] / 3,
          255,
        ]),
      );
    }
  }

  if mismatch_count == 0 {
    return Ok(());
  }

  // Apply the pixel-budget options. A run that exceeds the threshold
  // can still pass if the absolute or fractional budget is generous.
  if let Some(max_pixels) = options.max_diff_pixels
    && mismatch_count <= max_pixels
  {
    return Ok(());
  }
  if let Some(ratio) = options.max_diff_pixel_ratio {
    let allowed = (ratio.clamp(0.0, 1.0) * total_pixels as f64).round();
    // After clamp + round, allowed is in [0, total_pixels]. Compare
    // in f64 to avoid the sign-loss cast lint.
    if (mismatch_count as f64) <= allowed {
      return Ok(());
    }
  }

  let mismatch_pct = (mismatch_count as f64 / total_pixels as f64) * 100.0;

  let _ = std::fs::create_dir_all(snap_dir);
  let _ = diff_img.save(&diff_path);
  let _ = std::fs::write(&actual_path, actual_png);

  let mut diff_png = Vec::new();
  diff_img
    .write_to(&mut std::io::Cursor::new(&mut diff_png), image::ImageFormat::Png)
    .ok();

  Err(TestFailure {
    message: format!(
      "screenshot '{name}' mismatch: {mismatch_count}/{total_pixels} pixels differ ({mismatch_pct:.2}%)\n\
       threshold: {threshold}/255 per channel\n\
       expected: {}\n\
       actual:   {}\n\
       diff:     {}\n\
       Run with UPDATE_SNAPSHOTS=1 to update baseline.",
      snap_path.display(),
      actual_path.display(),
      diff_path.display(),
    ),
    stack: None,
    diff: None,
    screenshot: Some(diff_png),
  })
}

/// Map a Playwright-style `[0, 1]` threshold into the `[0, 255]` per-
/// channel byte difference the comparator uses internally. Saturating
/// conversion handled discretely so clippy's lossy/sign-loss casts
/// don't fire.
fn f64_threshold_to_u8(t: f64) -> u8 {
  // `(t.clamp(0.0, 1.0) * 255.0)` is in [0, 255]. Snap to a few u8
  // bands rather than bit-twiddling; the comparator only cares about
  // rough granularity.
  let scaled = (t.clamp(0.0, 1.0) * 255.0).round();
  for byte in 0u8..=255 {
    if f64::from(byte) >= scaled {
      return byte;
    }
  }
  255
}
