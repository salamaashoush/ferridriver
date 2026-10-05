use std::sync::{Arc, Mutex};

use crate::{RunContext, ScriptEngineConfig, ScriptError, Session, SessionProcs};

#[derive(Default)]
pub struct SessionResources {
  pub(crate) browsers: Arc<ferridriver::BrowserResources>,
  pub(crate) procs: Arc<SessionProcs>,
  state: Mutex<ResourceState>,
  closed: tokio::sync::Notify,
  idle: tokio::sync::Notify,
}

#[derive(Default)]
struct ResourceState {
  closing: bool,
  running: usize,
  finished: bool,
}

struct RunLease<'a>(&'a SessionResources);

impl Drop for RunLease<'_> {
  fn drop(&mut self) {
    let idle = {
      let mut state = self.0.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      state.running -= 1;
      state.running == 0
    };
    if idle {
      self.0.idle.notify_waiters();
    }
  }
}

struct Running<'a, F> {
  // Drop the runtime future before releasing its lease: its cancellation
  // guard must poison the VM before close can observe an idle session.
  future: std::pin::Pin<Box<F>>,
  _lease: RunLease<'a>,
}

impl SessionResources {
  pub async fn create(
    self: &Arc<Self>,
    config: ScriptEngineConfig,
    context: &RunContext,
  ) -> Result<Session, ScriptError> {
    self.ensure_open()?;
    let session = self
      .run_until_closed(Session::create_with_resources(config, context, Arc::clone(self)))
      .await??;
    self.ensure_open()?;
    Ok(session)
  }

  pub(crate) fn ensure_open(&self) -> Result<(), ScriptError> {
    if self
      .state
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .closing
    {
      Err(ScriptError::named("TargetClosedError", "script session is closing"))
    } else {
      Ok(())
    }
  }

  pub async fn close(&self) -> Result<(), ScriptError> {
    self.close_browsers().await?;
    self.procs.close().await.map_err(ScriptError::internal)?;
    self
      .state
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .finished = true;
    Ok(())
  }

  pub(crate) async fn close_browsers(&self) -> Result<(), ScriptError> {
    self.begin_close();
    self.wait_for_runs().await;
    self
      .browsers
      .close()
      .await
      .map_err(|error| ScriptError::internal(error.to_string()))
  }

  pub(crate) fn begin_close(&self) {
    self
      .state
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .closing = true;
    self.browsers.begin_close();
    self.procs.begin_close();
    self.closed.notify_waiters();
  }

  pub(crate) async fn run_until_closed<T>(&self, run: impl std::future::Future<Output = T>) -> Result<T, ScriptError> {
    let lease = {
      let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      if state.closing {
        return Err(ScriptError::named("TargetClosedError", "script session is closing"));
      }
      state.running += 1;
      RunLease(self)
    };
    let mut running = Running {
      future: Box::pin(run),
      _lease: lease,
    };
    let result = tokio::select! {
      biased;
      () = self.cancelled() => Err(ScriptError::named("TargetClosedError", "script session is closing")),
      result = &mut running.future => Ok(result),
    };
    drop(running);
    result
  }

  async fn cancelled(&self) {
    let notification = self.closed.notified();
    tokio::pin!(notification);
    notification.as_mut().enable();
    if self.ensure_open().is_ok() {
      notification.await;
    }
  }

  async fn wait_for_runs(&self) {
    loop {
      let notification = self.idle.notified();
      tokio::pin!(notification);
      notification.as_mut().enable();
      if self
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .running
        == 0
      {
        return;
      }
      notification.await;
    }
  }
}

async fn close_owners(browsers: &ferridriver::BrowserResources, procs: &SessionProcs) -> Result<(), ScriptError> {
  // A command can host WebDriver/Appium. Keep it available for DELETE and
  // its retries; process dependencies cannot be inferred from command names.
  browsers
    .close()
    .await
    .map_err(|error| ScriptError::internal(error.to_string()))?;
  procs.close().await.map_err(ScriptError::internal)
}

impl Drop for SessionResources {
  fn drop(&mut self) {
    if self
      .state
      .get_mut()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .finished
    {
      return;
    }
    let browsers = Arc::clone(&self.browsers);
    let procs = Arc::clone(&self.procs);
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      runtime.spawn(async move {
        if let Err(error) = close_owners(&browsers, &procs).await {
          tracing::warn!(error = %error.message, "final script resource cleanup failed");
        }
      });
    } else {
      tracing::warn!("script resources dropped without a runtime for cleanup");
    }
  }
}
