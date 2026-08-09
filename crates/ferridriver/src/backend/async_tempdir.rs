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
use std::sync::Mutex;

pub struct AsyncTempDir {
  /// The path to remove, taken by whichever of [`AsyncTempDir::remove_now`]
  /// or `Drop` runs first. Behind a `Mutex` because the browser handle
  /// is shared through an `Arc`: a `close()` on any clone must be able
  /// to reclaim the directory without waiting for the last clone to go
  /// away, and `Drop` must then be a no-op.
  path: Mutex<Option<PathBuf>>,
}

impl AsyncTempDir {
  pub fn new(inner: tempfile::TempDir) -> Self {
    // `keep` consumes the `TempDir` and disables its auto-removal,
    // handing back the raw `PathBuf` so we own the scheduling.
    Self {
      path: Mutex::new(Some(inner.keep())),
    }
  }

  fn take(&self) -> Option<PathBuf> {
    self
      .path
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
  }

  /// Remove the directory now, off the async worker thread, and wait
  /// for it. Called from the browser's `close()` so teardown does not
  /// depend on `Drop` running — a process killed by a signal never
  /// drops anything, and a Chromium profile dir is megabytes.
  pub async fn remove_now(&self) {
    let Some(path) = self.take() else {
      return;
    };
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&path)).await;
  }
}

impl Drop for AsyncTempDir {
  fn drop(&mut self) {
    let Some(path) = self.take() else {
      return;
    };
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
