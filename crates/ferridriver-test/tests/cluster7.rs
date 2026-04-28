#![allow(clippy::expect_used, clippy::unwrap_used, clippy::items_after_statements)]
//! Cluster 7 — `git_info` helper unit coverage (§7.26 + §7.3 plumbing).

use ferridriver_test::git_info::GitInfo;

#[test]
fn capture_returns_a_record_without_panicking() {
  let info = GitInfo::capture();
  // We can't assert a specific commit because the workspace's HEAD
  // moves, but `capture` must always return a record.
  let _ = info.commit;
  let _ = info.branch;
}

#[test]
fn changed_files_with_empty_ref_returns_porcelain_paths() {
  // No assumption about what's dirty — the helper just shouldn't
  // panic and must return either Some(_) (in a git repo) or None
  // (e.g. when git is not on PATH).
  let _ = GitInfo::changed_files("");
}

#[test]
fn changed_files_with_invalid_ref_returns_none() {
  // git diff against a nonsense ref will fail; the helper must
  // gracefully degrade rather than panic.
  let result = GitInfo::changed_files("definitely-not-a-real-ref-xyz");
  assert!(result.is_none(), "invalid ref should return None");
}
