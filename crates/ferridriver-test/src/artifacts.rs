//! Where a run keeps the files it is still writing.
//!
//! Finished artifacts (traces, screenshots, videos) live under the test's
//! own output directory. In-progress ones cannot: a trace being recorded
//! is a directory of loose files that a viewer reads while the test runs,
//! and it has to be somewhere predictable, per worker, and swept
//! afterwards.
//!
//! The directory name is not ours to choose. The embedded trace viewer
//! computes a running test's trace path itself, from the output directory
//! and the worker index (`folders.ts::artifactsFolderName`), so anything
//! else would leave its live view permanently empty.

use std::path::{Path, PathBuf};

/// Per-worker scratch directory under `output_dir`.
#[must_use]
pub fn artifacts_dir(output_dir: &Path, worker_index: usize) -> PathBuf {
  output_dir.join(format!(".playwright-artifacts-{worker_index}"))
}

/// Where a worker's in-progress trace files go.
#[must_use]
pub fn traces_dir(output_dir: &Path, worker_index: usize) -> PathBuf {
  artifacts_dir(output_dir, worker_index).join("traces")
}

/// Remove every worker scratch directory under `output_dir`.
///
/// Called at the start of a run (a previous run killed mid-test leaves
/// its half-written traces behind) and at the end of one (the traces
/// worth keeping have been zipped into the tests' output directories by
/// then).
pub fn sweep(output_dir: &Path) {
  let Ok(entries) = std::fs::read_dir(output_dir) else {
    return;
  };
  for entry in entries.flatten() {
    let name = entry.file_name();
    let Some(name) = name.to_str() else { continue };
    if name.starts_with(".playwright-artifacts-") {
      let _ = std::fs::remove_dir_all(entry.path());
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn layout_matches_what_the_viewer_looks_for() {
    let dir = artifacts_dir(Path::new("/runs/test-results"), 3);
    assert!(dir.ends_with(".playwright-artifacts-3"));
    assert!(traces_dir(Path::new("/runs/test-results"), 3).ends_with(".playwright-artifacts-3/traces"));
  }

  #[test]
  fn sweep_removes_only_worker_scratch_directories() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(root.path().join(".playwright-artifacts-0/traces")).expect("mkdir");
    std::fs::create_dir_all(root.path().join(".playwright-artifacts-12")).expect("mkdir");
    std::fs::create_dir_all(root.path().join("suite-test")).expect("mkdir");
    std::fs::write(root.path().join("suite-test/trace.zip"), b"PK").expect("write");

    sweep(root.path());

    assert!(!root.path().join(".playwright-artifacts-0").exists());
    assert!(!root.path().join(".playwright-artifacts-12").exists());
    assert!(
      root.path().join("suite-test/trace.zip").exists(),
      "kept artifacts deleted"
    );
  }
}
