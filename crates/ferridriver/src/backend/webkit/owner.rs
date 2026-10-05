use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::connection::Connection;
use crate::backend::{async_tempdir::AsyncTempDir, process::ChildGroup};

pub(crate) struct WebKitOwner {
  child: Arc<tokio::sync::Mutex<Option<ChildGroup>>>,
  connection: Arc<Mutex<Option<Arc<Connection>>>>,
  downloads: Arc<AsyncTempDir>,
  closing: AtomicBool,
  observers: std::sync::OnceLock<crate::backend::transport_tasks::TransportTasks>,
  close_mode: Mutex<CloseMode>,
}

enum CloseMode {
  Open,
  Requested(tokio::time::Instant),
  Forced,
}

impl WebKitOwner {
  pub(super) fn new(child: ChildGroup, downloads: tempfile::TempDir) -> Arc<Self> {
    let owner = Arc::new(Self {
      child: Arc::new(tokio::sync::Mutex::new(Some(child))),
      connection: Arc::new(Mutex::new(None)),
      downloads: Arc::new(AsyncTempDir::new(downloads)),
      closing: AtomicBool::new(false),
      observers: std::sync::OnceLock::new(),
      close_mode: Mutex::new(CloseMode::Open),
    });
    crate::state::allocation::retain_webkit(&owner);
    owner
  }

  pub(super) fn install_connection(&self, connection: Arc<Connection>) {
    *self
      .connection
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(connection);
  }

  pub(super) fn is_running(&self) -> bool {
    !self.closing.load(Ordering::Acquire)
      && self
        .child
        .try_lock()
        .map_or(true, |mut child| child.as_mut().is_some_and(ChildGroup::is_running))
  }

  pub(super) fn install_observers(&self, tasks: crate::backend::transport_tasks::TransportTasks) {
    if self.observers.set(tasks).is_err() {
      tracing::error!("WebKit observers were installed more than once");
    }
  }

  pub(crate) fn cleanup_complete(&self) -> bool {
    self.child.try_lock().is_ok_and(|child| child.is_none())
      && self
        .connection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_none()
      && self.downloads.cleanup_complete()
  }

  pub(crate) async fn close(&self) -> crate::Result<()> {
    self.closing.store(true, Ordering::Release);
    let deadline = self.request_close();
    let observers = match self.observers.get() {
      Some(tasks) => tasks.close().await,
      None => Ok(()),
    };
    let resources = cleanup(&self.child, &self.connection, &self.downloads, deadline).await;
    observers?;
    resources
  }

  fn request_close(&self) -> Option<tokio::time::Instant> {
    let mut mode = self
      .close_mode
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(*mode, CloseMode::Open) {
      let requested = self
        .connection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|conn| conn.request_browser_close().is_ok());
      *mode = if requested {
        CloseMode::Requested(tokio::time::Instant::now() + std::time::Duration::from_secs(3))
      } else {
        CloseMode::Forced
      };
    }
    match *mode {
      CloseMode::Requested(deadline) => Some(deadline),
      CloseMode::Open | CloseMode::Forced => None,
    }
  }
}

async fn cleanup(
  child: &tokio::sync::Mutex<Option<ChildGroup>>,
  connection: &Mutex<Option<Arc<Connection>>>,
  downloads: &AsyncTempDir,
  deadline: Option<tokio::time::Instant>,
) -> crate::Result<()> {
  let conn = connection
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  let process = async {
    let mut child = child.lock().await;
    if let Some(group) = child.as_mut() {
      if let Some(deadline) = deadline {
        match tokio::time::timeout_at(deadline, group.wait()).await {
          Ok(result) => {
            result?;
          },
          Err(_) => group.shutdown().await?,
        }
      } else {
        group.shutdown().await?;
      }
    }
    *child = None;
    Ok::<(), crate::FerriError>(())
  }
  .await;
  let transport = if let Some(conn) = conn {
    let result = conn.close().await;
    if result.is_ok() {
      connection
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    }
    result
  } else {
    Ok(())
  };
  process?;
  transport?;
  downloads.remove_now().await
}

impl Drop for WebKitOwner {
  fn drop(&mut self) {
    if self.cleanup_complete() {
      return;
    }
    if let Some(conn) = self
      .connection
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .as_ref()
    {
      conn.start_close();
    }
    let child = Arc::clone(&self.child);
    let connection = Arc::clone(&self.connection);
    let downloads = Arc::clone(&self.downloads);
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      runtime.spawn(async move {
        if let Err(error) = cleanup(&child, &connection, &downloads, None).await {
          tracing::warn!(%error, "final WebKit resource cleanup failed");
        }
      });
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn owner() -> (Arc<WebKitOwner>, std::path::PathBuf) {
    let downloads = tempfile::tempdir().unwrap();
    let path = downloads.path().to_owned();
    let child = tokio::process::Command::new("sh")
      .args(["-c", "exec sleep 30"])
      .process_group(0)
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    (WebKitOwner::new(ChildGroup::unregistered(child), downloads), path)
  }

  #[tokio::test]
  async fn cancelled_close_retains_the_child_and_downloads_for_retry() {
    let (owner, path) = owner();
    let child = owner.child.lock().await;
    let mut close = Box::pin(owner.close());
    assert!(futures::poll!(&mut close).is_pending());
    drop(close);
    assert!(child.is_some());
    assert!(path.exists());
    drop(child);
    owner.close().await.unwrap();
    assert!(owner.cleanup_complete());
    assert!(!path.exists());
  }

  #[tokio::test]
  async fn failed_download_removal_retains_the_directory_owner_for_retry() {
    let (owner, path) = owner();
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, b"sashoush owned download fixture").unwrap();
    assert!(owner.close().await.is_err());
    assert!(!owner.cleanup_complete());
    assert!(owner.child.lock().await.is_none());
    assert_eq!(std::fs::read(&path).unwrap(), b"sashoush owned download fixture");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    owner.close().await.unwrap();
    assert!(owner.cleanup_complete());
    assert!(!path.exists());
  }

  #[tokio::test]
  async fn cancelled_close_preserves_the_original_graceful_exit_window() {
    use tokio::io::AsyncBufReadExt;
    let (owner, _) = owner();
    let (read, _input) = tokio::io::duplex(1024);
    let (write, output) = tokio::io::duplex(1024);
    let connection = Connection::spawn(super::super::transport::Transport::new(read, write));
    owner.install_connection(connection);
    let mut closing = Box::pin(owner.close());
    assert!(futures::poll!(&mut closing).is_pending());
    drop(closing);
    let mut output = tokio::io::BufReader::new(output);
    let mut frame = Vec::new();
    output.read_until(0, &mut frame).await.unwrap();
    frame.pop();
    let request: serde_json::Value = serde_json::from_slice(&frame).unwrap();
    assert_eq!(request["method"], "Playwright.close");
    assert!(
      tokio::time::timeout(std::time::Duration::from_millis(100), owner.close())
        .await
        .is_err()
    );
    {
      let mut child = owner.child.lock().await;
      let group = child.as_mut().expect("retry must retain the child");
      assert!(
        group.is_running(),
        "retry forced termination before the graceful deadline"
      );
      group.terminate().unwrap();
    }
    owner.close().await.unwrap();
    assert!(owner.cleanup_complete());
  }
}
