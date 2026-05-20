//! [`AsyncTempDir`] — a `tempfile::TempDir` whose `Drop` removes the
//! directory off the tokio worker thread instead of blocking it.
//!
//! Chromium user-data-dirs accumulate megabytes of profile state
//! (`IndexedDB`, code cache, browser cache). `tempfile::TempDir::drop`
//! runs `std::fs::remove_dir_all` synchronously on whichever thread
//! holds the last `Arc`, which is typically a tokio worker. On a
//! multi-worker run that means N concurrent blocking removals on the
//! shared async runtime threadpool. `AsyncTempDir` defers the removal
//! to `tokio::task::spawn_blocking` if a runtime is active, falling
//! back to a sync removal when not (e.g. test harness teardown).

use std::path::PathBuf;

pub struct AsyncTempDir {
  /// `None` only between `into_path` and `Drop` — invariant `Some`
  /// when constructed.
  inner: Option<tempfile::TempDir>,
}

impl AsyncTempDir {
  pub fn new(inner: tempfile::TempDir) -> Self {
    Self { inner: Some(inner) }
  }
}

impl Drop for AsyncTempDir {
  fn drop(&mut self) {
    let Some(inner) = self.inner.take() else {
      return;
    };
    // `into_path` consumes the `TempDir` and disables its auto-removal,
    // handing back the raw `PathBuf` so we can schedule the rm
    // ourselves.
    let path: PathBuf = inner.keep();
    // Try to defer to the tokio blocking pool. If we're not inside a
    // runtime (e.g. plain `#[test]`), fall back to a sync removal so
    // the directory still gets cleaned up.
    match tokio::runtime::Handle::try_current() {
      Ok(handle) => {
        handle.spawn_blocking(move || {
          let _ = std::fs::remove_dir_all(&path);
        });
      },
      Err(_) => {
        let _ = std::fs::remove_dir_all(&path);
      },
    }
  }
}
