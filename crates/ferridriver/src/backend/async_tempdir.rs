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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct AsyncTempDir {
  state: Arc<RemovalState>,
}

struct RemovalState {
  path: Mutex<Option<PathBuf>>,
}

impl AsyncTempDir {
  pub(crate) fn cleanup_complete(&self) -> bool {
    self.state.path.try_lock().is_ok_and(|path| path.is_none())
  }

  pub fn new(inner: tempfile::TempDir) -> Self {
    // `keep` consumes the `TempDir` and disables its auto-removal,
    // handing back the raw `PathBuf` so we own the scheduling.
    Self {
      state: Arc::new(RemovalState {
        path: Mutex::new(Some(inner.keep())),
      }),
    }
  }

  /// Remove the directory now, off the async worker thread, and wait
  /// for it. Called from the browser's `close()` so teardown does not
  /// depend on `Drop` running — a process killed by a signal never
  /// drops anything, and a Chromium profile dir is megabytes.
  ///
  /// # Errors
  /// Failed filesystem removal or a failed cleanup task leaves the path retained for another attempt.
  pub async fn remove_now(&self) -> crate::Result<()> {
    self.remove_with(|path| std::fs::remove_dir_all(path)).await
  }

  async fn remove_with(&self, remove: impl FnOnce(&Path) -> std::io::Result<()> + Send + 'static) -> crate::Result<()> {
    let state = Arc::clone(&self.state);
    // The blocking job keeps ownership and serialization even when its caller is cancelled.
    tokio::task::spawn_blocking(move || {
      let (path, error) = {
        let mut owned = state.path.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(path) = owned.as_deref() else {
          return Ok(());
        };
        match removal_result(path, remove(path)) {
          Ok(()) => {
            *owned = None;
            return Ok(());
          },
          Err(error) => (path.to_owned(), error),
        }
      };
      tracing::warn!(%error, path = %path.display(), "Temporary directory removal failed");
      Err(error)
    })
    .await
    .map_err(|error| crate::FerriError::backend(format!("Temporary directory cleanup task failed: {error}")))??;
    Ok(())
  }
}

pub(crate) fn removal_result(path: &Path, result: std::io::Result<()>) -> std::io::Result<()> {
  match result {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => match std::fs::symlink_metadata(path) {
      Err(absent) if absent.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(other) => Err(other),
      Ok(_) => Err(error),
    },
    other => other,
  }
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
  fn drop(&mut self) {
    if let Err(error) = removal_result(&self.0, std::fs::remove_dir_all(&self.0)) {
      tracing::warn!(%error, path = %self.0.display(), "Temporary directory final cleanup failed");
    }
  }
}

impl Drop for RemovalState {
  fn drop(&mut self) {
    let Some(path) = self
      .path
      .get_mut()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take()
    else {
      return;
    };
    // Try to defer to the tokio blocking pool. If we're not inside a
    // runtime (e.g. plain `#[test]`), fall back to a sync removal so
    // the directory still gets cleaned up.
    let removal = RemoveOnDrop(path);
    match tokio::runtime::Handle::try_current() {
      Ok(handle) => {
        // A rejected or cancelled blocking job still drops its capture and performs cleanup.
        handle.spawn_blocking(move || drop(removal));
      },
      Err(_) => drop(removal),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn failed_removal_keeps_the_directory_for_an_explicit_retry() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().to_owned();
    let directory = AsyncTempDir::new(temporary);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, b"sashoush cleanup fixture").unwrap();
    let error = directory.remove_now().await;
    let retained = directory.state.path.lock().unwrap().is_some();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    directory.remove_now().await.unwrap();
    let removed = !path.exists();
    if !removed {
      std::fs::remove_dir(&path).unwrap();
    }
    assert!(error.is_err());
    assert!(retained, "failed removal discarded the owned path");
    assert!(removed, "retry returned before removing the owned directory");
  }

  #[tokio::test]
  async fn an_already_absent_directory_is_successful_and_idempotent() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().to_owned();
    let directory = AsyncTempDir::new(temporary);
    std::fs::remove_dir(&path).unwrap();
    directory.remove_now().await.unwrap();
    directory.remove_now().await.unwrap();
    assert!(directory.state.path.lock().unwrap().is_none());
    assert!(!path.exists());
  }

  #[tokio::test]
  async fn a_missing_child_does_not_confirm_removal_of_the_root() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().to_owned();
    let directory = AsyncTempDir::new(temporary);
    assert!(
      directory
        .remove_with(|_| Err(std::io::ErrorKind::NotFound.into()))
        .await
        .is_err()
    );
    assert!(directory.state.path.lock().unwrap().is_some());
    assert!(path.is_dir());
    directory.remove_now().await.unwrap();
    assert!(!path.exists());
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn cancelled_callers_keep_active_removal_serialized_and_record_its_completion() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().to_owned();
    std::fs::write(path.join("owned"), b"sashoush").unwrap();
    let directory = Arc::new(AsyncTempDir::new(temporary));
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let first = Arc::clone(&directory);
    let caller = tokio::spawn(async move {
      first
        .remove_with(move |path| {
          started.send(()).unwrap();
          released.blocking_recv().unwrap();
          std::fs::remove_dir_all(path)
        })
        .await
    });
    running.await.unwrap();
    assert!(directory.state.path.try_lock().is_err());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(path.join("owned").exists());
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = attempts.clone();
    let retrying = Arc::clone(&directory);
    let (queued, receipt) = tokio::sync::oneshot::channel();
    let retry = tokio::spawn(async move {
      let mut removal = std::pin::pin!(retrying.remove_with(move |path| {
        observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::fs::remove_dir_all(path)
      }));
      assert!(futures::poll!(&mut removal).is_pending());
      queued.send(()).unwrap();
      removal.await
    });
    receipt.await.unwrap();
    assert!(directory.state.path.try_lock().is_err());
    drop(directory);
    release.send(()).unwrap();
    retry.await.unwrap().unwrap();
    assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(!path.exists());
  }

  #[test]
  fn dropping_without_a_runtime_removes_the_owned_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().to_owned();
    drop(AsyncTempDir::new(temporary));
    assert!(!path.exists());
  }
}
