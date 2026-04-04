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
pub fn assert_snapshot(
  test_info: &TestInfo,
  actual: &str,
  name: &str,
  update: bool,
) -> Result<(), TestFailure> {
  let snap_path = snapshot_path(&test_info.snapshot_dir, &test_info.test_id.full_name(), name);

  if update || !snap_path.exists() {
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

/// Compute the snapshot file path.
fn snapshot_path(snapshot_dir: &Path, test_full_name: &str, snap_name: &str) -> PathBuf {
  let sanitized = test_full_name
    .replace(['/', '\\', ':', '<', '>', '"', '|', '?', '*'], "_")
    .replace(' ', "_");
  snapshot_dir.join(sanitized).join(format!("{snap_name}.snap"))
}
