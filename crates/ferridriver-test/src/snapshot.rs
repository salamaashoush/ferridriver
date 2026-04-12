//! Text snapshot testing: save expected output to `.snap` files, diff on mismatch.
//!
//! ```ignore
//! use ferridriver_test::snapshot::assert_snapshot;
//!
//! let info: Arc<TestInfo> = pool.get("test_info").await?;
//! assert_snapshot(&info, &page.content().await?, "page-content", false).await?;
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
pub fn assert_snapshot(test_info: &TestInfo, actual: &str, name: &str, update: bool) -> Result<(), TestFailure> {
  use crate::config::UpdateSnapshotsMode;

  let snap_path = if let Some(ref template) = test_info.snapshot_path_template {
    resolve_template_path(
      template,
      &test_info.test_id.file,
      &test_info.test_id.full_name(),
      &test_info.snapshot_dir,
      name,
      ".snap",
    )
  } else {
    snapshot_path(&test_info.snapshot_dir, &test_info.test_id.full_name(), name)
  };

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

/// Compare a PNG screenshot against a stored baseline, producing pixel-level diff.
///
/// If `UPDATE_SNAPSHOTS=1` is set or baseline doesn't exist, saves the actual as baseline.
/// On mismatch, saves actual and diff images alongside the baseline.
///
/// # Errors
///
/// Returns `TestFailure` with diff details if screenshots don't match.
pub fn compare_screenshot_png(actual_png: &[u8], name: &str) -> Result<(), TestFailure> {
  let snap_dir = PathBuf::from("__snapshots__");
  let update = std::env::var("UPDATE_SNAPSHOTS").is_ok();
  let snap_path = snap_dir.join(format!("{name}.png"));
  let diff_path = snap_dir.join(format!("{name}-diff.png"));
  let actual_path = snap_dir.join(format!("{name}-actual.png"));

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

  let threshold: u8 = std::env::var("SCREENSHOT_THRESHOLD")
    .ok()
    .and_then(|v| v.parse().ok())
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

  let mismatch_pct = (mismatch_count as f64 / total_pixels as f64) * 100.0;

  let _ = std::fs::create_dir_all(&snap_dir);
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

/// Compute the snapshot file path.
fn snapshot_path(snapshot_dir: &Path, test_full_name: &str, snap_name: &str) -> PathBuf {
  let sanitized = test_full_name
    .replace(['/', '\\', ':', '<', '>', '"', '|', '?', '*'], "_")
    .replace(' ', "_");
  snapshot_dir.join(sanitized).join(format!("{snap_name}.snap"))
}

/// Resolve a snapshot path using a Playwright-style template.
///
/// Supported placeholders:
/// - `{testDir}` — directory containing the test file
/// - `{snapshotDir}` — configured snapshot directory
/// - `{snapshotSuffix}` — empty (platform suffix, not used)
/// - `{testFileDir}` — relative directory of the test file
/// - `{testFileName}` — test file name without extension
/// - `{testFilePath}` — relative test file path without extension
/// - `{testName}` — sanitized test name (including suite hierarchy)
/// - `{arg}` — snapshot argument name
/// - `{ext}` — file extension (e.g. `.snap`, `.png`)
///
/// Example template: `{testDir}/__snapshots__/{testFilePath}/{arg}{ext}`
pub fn resolve_template_path(
  template: &str,
  test_file: &str,
  test_name: &str,
  snapshot_dir: &Path,
  arg: &str,
  ext: &str,
) -> PathBuf {
  let test_file_path = Path::new(test_file);
  let test_dir = test_file_path.parent().unwrap_or(Path::new(".")).to_string_lossy();
  let test_file_name = test_file_path.file_stem().unwrap_or_default().to_string_lossy();
  let test_file_no_ext = test_file_path.with_extension("").to_string_lossy().into_owned();

  let sanitized_name = test_name
    .replace(['/', '\\', ':', '<', '>', '"', '|', '?', '*'], "_")
    .replace(' ', "_");

  let resolved = template
    .replace("{testDir}", &test_dir)
    .replace("{snapshotDir}", &snapshot_dir.to_string_lossy())
    .replace("{snapshotSuffix}", "")
    .replace("{testFileDir}", &test_dir)
    .replace("{testFileName}", &test_file_name)
    .replace("{testFilePath}", &test_file_no_ext)
    .replace("{testName}", &sanitized_name)
    .replace("{arg}", arg)
    .replace("{ext}", ext);

  PathBuf::from(resolved)
}
