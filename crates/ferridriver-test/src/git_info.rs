//! Capture git metadata for `captureGitInfo` (§7.26).
//!
//! Returns a `GitInfo` snapshot built from `git rev-parse HEAD`,
//! `git symbolic-ref --short HEAD`, and `git status --porcelain` so
//! reporters can annotate test results with the run's git context.
//! Outside of a git repo the helper returns a default record with
//! every field empty rather than failing the run.

use serde::{Deserialize, Serialize};
use std::process::Command;

/// Minimal git metadata surfaced via `RunSummary.metadata.git`. Each
/// field is best-effort — missing data is rendered as empty strings
/// so the JSON shape stays predictable regardless of repo state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitInfo {
  /// Full commit hash of `HEAD`. Empty when outside a git repo or
  /// the branch has no commits.
  pub commit: String,
  /// Symbolic branch name. Empty in detached-HEAD state.
  pub branch: String,
  /// `true` when the worktree has uncommitted changes (porcelain
  /// status non-empty).
  pub dirty: bool,
}

impl GitInfo {
  /// Run `git` against the current working directory and return what
  /// the helper could collect. Never panics — every shell-out failure
  /// degrades to an empty field.
  pub fn capture() -> Self {
    let mut info = Self::default();
    if let Some(out) = run_git(&["rev-parse", "HEAD"]) {
      info.commit = out;
    }
    if let Some(out) = run_git(&["symbolic-ref", "--short", "HEAD"]) {
      info.branch = out;
    }
    if let Some(out) = run_git(&["status", "--porcelain"]) {
      info.dirty = !out.is_empty();
    }
    info
  }

  /// Files changed between `HEAD` and `reference`. Used by
  /// `--only-changed`. `reference` may be empty (`""`), in which
  /// case the working-tree diff is returned. Returns `None` when
  /// `git` is unavailable so callers can skip the filter.
  pub fn changed_files(reference: &str) -> Option<Vec<String>> {
    let args: Vec<&str> = if reference.is_empty() {
      vec!["status", "--porcelain"]
    } else {
      vec!["diff", "--name-only", reference, "HEAD"]
    };
    let raw = run_git(&args)?;
    if reference.is_empty() {
      // Porcelain lines look like `XY <path>` with two-char status
      // followed by a space.
      let mut files = Vec::new();
      for line in raw.lines() {
        if line.len() <= 3 {
          continue;
        }
        let path = &line[3..];
        files.push(path.trim().to_string());
      }
      Some(files)
    } else {
      Some(
        raw
          .lines()
          .map(|s| s.trim().to_string())
          .filter(|s| !s.is_empty())
          .collect(),
      )
    }
  }
}

fn run_git(args: &[&str]) -> Option<String> {
  let out = Command::new("git").args(args).output().ok()?;
  if !out.status.success() {
    return None;
  }
  let stdout = String::from_utf8(out.stdout).ok()?;
  Some(stdout.trim().to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn capture_in_a_repo_returns_a_commit_or_empty_default() {
    // Smoke test: under cargo test the cwd is the workspace root,
    // which is a git repo. We only assert the helper doesn't panic
    // and returns a string-shaped value.
    let info = GitInfo::capture();
    // Either we picked up a commit or we degraded gracefully.
    let _ = info.commit.len();
  }
}
