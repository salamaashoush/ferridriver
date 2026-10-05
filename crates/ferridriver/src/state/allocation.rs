use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{Mutex, watch};

use super::{AnyBrowser, ConnectMode, FerriError, LaunchSpec, Result, connect_browser};
use crate::backend::webdriver::session::WebDriverSession;

tokio::task_local! {
  static CURRENT: Arc<Allocation>;
}

pub(crate) fn retain_session(session: &Arc<WebDriverSession>) {
  retain(PartialResource::Session(Arc::clone(session)));
}

pub(crate) fn retain_ios(owner: &Arc<crate::ios::IosOwner>) {
  retain(PartialResource::Ios(Arc::clone(owner)));
}

pub(crate) fn retain_android(owner: &Arc<crate::android::AndroidOwner>) {
  retain(PartialResource::Android(Arc::clone(owner)));
}

pub(crate) fn retain_driver(owner: &Arc<Mutex<Option<crate::backend::process::ChildGroup>>>) {
  retain(PartialResource::Driver(Arc::clone(owner)));
}

pub(crate) fn retain_webkit(owner: &Arc<crate::backend::webkit::owner::WebKitOwner>) {
  retain(PartialResource::WebKit(Arc::clone(owner)));
}

fn retain(resource: PartialResource) {
  let _ = CURRENT.try_with(|allocation| {
    allocation
      .partial
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(resource);
  });
}

pub(crate) fn has_pending_sessions() -> bool {
  CURRENT
    .try_with(|allocation| {
      allocation
        .partial
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|resource| matches!(resource, PartialResource::Session(session) if !session.deletion_complete()))
    })
    .unwrap_or(false)
}

#[derive(Clone)]
enum PartialResource {
  Session(Arc<WebDriverSession>),
  Driver(Arc<Mutex<Option<crate::backend::process::ChildGroup>>>),
  Ios(Arc<crate::ios::IosOwner>),
  Android(Arc<crate::android::AndroidOwner>),
  WebKit(Arc<crate::backend::webkit::owner::WebKitOwner>),
}

impl PartialResource {
  fn cleanup_complete(&self) -> bool {
    match self {
      Self::Session(session) => session.deletion_complete(),
      Self::Driver(driver) => driver.try_lock().is_ok_and(|driver| driver.is_none()),
      Self::Ios(owner) => owner.cleanup_complete(),
      Self::Android(owner) => owner.cleanup_complete(),
      Self::WebKit(owner) => owner.cleanup_complete(),
    }
  }

  async fn close(&self) -> Result<()> {
    match self {
      Self::Session(session) => session.close().await,
      Self::Driver(driver) => {
        let mut driver = driver.lock().await;
        if let Some(group) = driver.as_mut() {
          group.shutdown().await?;
        }
        *driver = None;
        Ok(())
      },
      Self::Ios(owner) => owner.close().await,
      Self::Android(owner) => owner.close().await,
      Self::WebKit(owner) => owner.close().await,
    }
  }
}

pub(super) struct Allocation {
  browser: Mutex<Option<AnyBrowser>>,
  done: watch::Sender<bool>,
  closing: AtomicBool,
  connection_budget: std::sync::OnceLock<crate::operation_budget::OperationBudget>,
  partial: std::sync::Mutex<Vec<PartialResource>>,
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
    let partial = std::mem::take(
      self
        .partial
        .get_mut()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
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
      partial: std::sync::Mutex::new(Vec::new()),
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
    !self
      .partial
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_empty()
  }

  pub(super) async fn produce(self: &Arc<Self>, spec: LaunchSpec, instance: &str) -> Result<ConnectMode> {
    let result = CURRENT
      .scope(Arc::clone(self), Box::pin(self.produce_inner(spec, instance)))
      .await;
    let mut partial = self.partial.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if result.is_ok() {
      partial.clear();
    } else {
      partial.retain(|resource| !resource.cleanup_complete());
    }
    result
  }

  async fn produce_inner(&self, spec: LaunchSpec, instance: &str) -> Result<ConnectMode> {
    if self.is_closing() {
      return Err(Self::closed_error());
    }
    let (mode, effective) = Box::pin(spec.resolve_off_lock(instance)).await?;
    if self.is_closing() {
      return Err(Self::closed_error());
    }
    let browser = match &mode {
      ConnectMode::Launch => spec.launch_browser(&effective).await?,
      other => {
        let connect = connect_browser(
          other,
          effective.backend_kind,
          spec.connection_headers.as_ref(),
          &effective.args,
        );
        if matches!(other, ConnectMode::ConnectUrl(_) | ConnectMode::AutoConnect { .. }) {
          let requested = crate::operation_budget::OperationBudget::new(effective.launch_timeout.unwrap_or(30_000))?;
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
    let partial = self
      .partial
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    for resource in partial.iter().rev() {
      resource.close().await?;
    }
    self
      .partial
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::process::ChildGroup;
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
    let mut command = tokio::process::Command::new("sleep");
    command.arg("30").process_group(0);
    let driver = Arc::new(Mutex::new(Some(ChildGroup::new(command.spawn().unwrap()))));
    let (allocation, completion) = Allocation::new();
    CURRENT
      .scope(Arc::clone(&allocation), async {
        retain_driver(&driver);
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
    let child = tokio::process::Command::new("sleep")
      .arg("30")
      .process_group(0)
      .kill_on_drop(true)
      .spawn()
      .unwrap();
    let mut group = ChildGroup::new(child);
    group.own_dir(&directory);
    let driver = Arc::new(Mutex::new(Some(group)));
    let resource = PartialResource::Driver(Arc::clone(&driver));
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
