use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ferridriver_driver::allocation_resource::{self, AllocationResource, ResourceList};
use tokio::sync::{Mutex, watch};

use super::{AnyBrowser, ConnectMode, FerriError, LaunchSpec, Result, connect_browser};

pub(crate) fn retain_resource<R: AllocationResource + 'static>(resource: &Arc<R>) {
  allocation_resource::retain(resource);
}

pub(crate) fn has_pending_sessions() -> bool {
  allocation_resource::has_pending_sessions()
}

pub(super) struct Allocation {
  browser: Mutex<Option<AnyBrowser>>,
  done: watch::Sender<bool>,
  closing: AtomicBool,
  connection_budget: std::sync::OnceLock<crate::operation_budget::OperationBudget>,
  partial: Arc<ResourceList>,
}

pub(super) struct Completion(Arc<Allocation>);

pub(super) struct Start {
  pub spec: LaunchSpec,
  pub shutdown_generation: u64,
  pub generation: u64,
  pub owner: Arc<Allocation>,
  pub completion: Completion,
}

impl Start {
  pub(super) fn spawn(
    self,
    runtime: &tokio::runtime::Handle,
    state: Arc<tokio::sync::RwLock<super::BrowserState>>,
    instance: String,
    permit: Option<tokio::sync::OwnedMutexGuard<()>>,
  ) -> tokio::sync::oneshot::Receiver<Result<()>> {
    let (reply, completed) = tokio::sync::oneshot::channel();
    runtime.spawn(async move {
      let _permit = permit;
      let created = self.owner.produce(self.spec, &instance).await;
      // Shutdown may hold the state lock while waiting for this allocation.
      drop(self.completion);
      let result = Box::pin(super::BrowserState::finish_allocation(
        &state,
        &instance,
        self.shutdown_generation,
        self.generation,
        &self.owner,
        created,
      ))
      .await;
      if let Err(Err(error)) = reply.send(result) {
        tracing::debug!(%error, instance, "Browser initialization finished after its caller disconnected");
      }
    });
    completed
  }
}

impl Drop for Allocation {
  fn drop(&mut self) {
    // Only explicit close can retain failed cleanup for retry. Final drop is best-effort.
    let browser = self.browser.get_mut().take();
    let partial = self.partial.take_all();
    if browser.is_none() && partial.is_empty() {
      return;
    }
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      runtime.spawn(async move {
        if let Some(mut browser) = browser
          && let Err(error) = browser.close().await
        {
          tracing::warn!(%error, "Closing an abandoned browser allocation failed");
          return;
        }
        for resource in partial.iter().rev() {
          if let Err(error) = resource.close().await {
            tracing::warn!(%error, "Closing an abandoned partial allocation failed");
            return;
          }
        }
      });
    } else {
      tracing::warn!("Browser allocation dropped without a runtime for cleanup");
    }
  }
}

impl Drop for Completion {
  fn drop(&mut self) {
    self.0.done.send_replace(true);
  }
}

impl Allocation {
  pub(super) fn new() -> (Arc<Self>, Completion) {
    let owner = Arc::new(Self {
      browser: Mutex::new(None),
      done: watch::channel(false).0,
      closing: AtomicBool::new(false),
      connection_budget: std::sync::OnceLock::new(),
      partial: ResourceList::new(),
    });
    let completion = Completion(Arc::clone(&owner));
    (owner, completion)
  }

  pub(super) fn connection_budget(&self) -> Option<crate::operation_budget::OperationBudget> {
    self.connection_budget.get().copied()
  }

  pub(super) fn is_closing(&self) -> bool {
    self.closing.load(Ordering::Acquire)
  }

  pub(super) fn closed_error() -> FerriError {
    FerriError::target_closed(Some("browser closed while the instance was starting".into()))
  }

  pub(super) fn has_partial_resources(&self) -> bool {
    !self.partial.is_empty()
  }

  pub(super) async fn produce(self: &Arc<Self>, spec: LaunchSpec, instance: &str) -> Result<ConnectMode> {
    let result =
      allocation_resource::scope(Arc::clone(&self.partial), Box::pin(self.produce_inner(spec, instance))).await;
    if result.is_ok() {
      self.partial.clear();
    } else {
      self.partial.retain_incomplete();
    }
    result
  }

  async fn produce_inner(&self, spec: LaunchSpec, instance: &str) -> Result<ConnectMode> {
    if self.is_closing() {
      return Err(Self::closed_error());
    }
    let (mode, mut effective) = Box::pin(spec.resolve_off_lock(instance)).await?;
    if self.is_closing() {
      return Err(Self::closed_error());
    }
    let browser = match &mode {
      ConnectMode::Launch => {
        let relay = spec.relay_launch_proxy(&mut effective).await?;
        let browser = spec.launch_browser(&effective).await?;
        spec.record_launched_proxy(instance, effective.launched_proxy.as_ref(), relay);
        browser
      },
      other => {
        let connect = connect_browser(
          other,
          effective.backend_kind,
          spec.connection_headers.as_ref(),
          &effective.args,
        );
        if matches!(
          other,
          ConnectMode::ConnectUrl(_) | ConnectMode::ConnectUrlWith { .. } | ConnectMode::AutoConnect { .. }
        ) {
          let timeout = match other {
            ConnectMode::ConnectUrlWith {
              timeout: Some(timeout), ..
            } => Some(*timeout),
            _ => effective.launch_timeout,
          };
          let requested = crate::operation_budget::OperationBudget::new(timeout.unwrap_or(30_000))?;
          let budget = *self.connection_budget.get_or_init(|| requested);
          budget
            .scope(budget.wait(connect))
            .await
            .map_err(|error| error.error("connecting browser"))??
        } else {
          connect.await?
        }
      },
    };
    *self.browser.lock().await = Some(browser);
    Ok(mode)
  }

  pub(super) async fn take_browser(&self) -> Result<AnyBrowser> {
    self
      .browser
      .lock()
      .await
      .take()
      .ok_or_else(|| FerriError::backend("browser allocation did not produce a browser"))
  }

  pub(super) async fn close(&self) -> Result<()> {
    self.closing.store(true, Ordering::Release);
    self
      .done
      .subscribe()
      .wait_for(|done| *done)
      .await
      .map_err(|error| FerriError::backend(error.to_string()))?;
    let mut browser = self.browser.lock().await;
    if let Some(browser) = browser.as_mut() {
      browser.close().await?;
    }
    *browser = None;
    let partial = self.partial.snapshot();
    for resource in partial.iter().rev() {
      resource.close().await?;
    }
    self.partial.clear();
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::process::ChildGroup;
  use crate::backend::webdriver::launcher::DriverResource;
  use crate::backend::webdriver::session::WebDriverSession;
  use crate::backend::webdriver::session::tests::request;
  use serde_json::json;
  use tokio::io::AsyncWriteExt as _;

  #[tokio::test]
  async fn a_partial_session_keeps_its_driver_until_deletion_succeeds() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/session", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      for (status, value) in [
        ("200 OK", json!({"sessionId":"partial"})),
        (
          "500 Internal Server Error",
          json!({"error":"unknown error","message":"retry later"}),
        ),
        (
          "500 Internal Server Error",
          json!({"error":"unknown error","message":"still unavailable"}),
        ),
        ("200 OK", serde_json::Value::Null),
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(request(&mut socket).await.0);
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
      }
      requests
    });
    let mut command = tokio::process::Command::from(crate::backend::process::test_sleeper(30));
    let driver = Arc::new(Mutex::new(Some(ChildGroup::new(command.spawn().unwrap()))));
    let (allocation, completion) = Allocation::new();
    allocation_resource::scope(Arc::clone(&allocation.partial), async {
      retain_resource(&Arc::new(DriverResource(Arc::clone(&driver))));
      let error = WebDriverSession::create(reqwest::Client::new(), endpoint.parse().unwrap(), json!({}), 1000, None)
        .await
        .err()
        .unwrap();
      assert!(error.to_string().contains("cleanup failed"), "{error}");
      assert!(has_pending_sessions());
    })
    .await;
    drop(completion);
    assert!(driver.lock().await.as_mut().unwrap().is_running());
    assert!(
      allocation
        .close()
        .await
        .unwrap_err()
        .to_string()
        .contains("still unavailable")
    );
    assert!(driver.lock().await.as_mut().unwrap().is_running());
    assert!(allocation.has_partial_resources());
    allocation.close().await.unwrap();
    assert!(driver.lock().await.is_none());
    assert!(!allocation.has_partial_resources());
    assert_eq!(
      server.await.unwrap(),
      [
        "POST /session HTTP/1.1",
        "DELETE /session/partial HTTP/1.1",
        "DELETE /session/partial HTTP/1.1",
        "DELETE /session/partial HTTP/1.1",
      ]
    );
  }
  #[tokio::test]
  async fn failed_driver_process_cleanup_keeps_the_same_owner_for_retry() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("profile");
    std::fs::write(&directory, b"sashoush cleanup fixture").unwrap();
    let child = tokio::process::Command::from(crate::backend::process::test_sleeper(30))
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let mut group = ChildGroup::new(child);
    group.own_dir(&directory);
    let driver = Arc::new(Mutex::new(Some(group)));
    let resource = DriverResource(Arc::clone(&driver));
    assert!(resource.close().await.is_err());
    assert!(driver.lock().await.is_some(), "failed cleanup discarded its owner");
    assert!(!resource.cleanup_complete());
    assert_eq!(std::fs::read(&directory).unwrap(), b"sashoush cleanup fixture");
    std::fs::remove_file(&directory).unwrap();
    std::fs::create_dir(&directory).unwrap();
    resource.close().await.unwrap();
    assert!(driver.lock().await.is_none());
    assert!(resource.cleanup_complete());
    assert!(!directory.exists(), "cleanup returned before removal completed");
  }
}
