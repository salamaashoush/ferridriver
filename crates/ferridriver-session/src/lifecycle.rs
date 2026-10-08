use std::sync::Arc;

use tokio::sync::{Mutex, RwLock, watch};

use crate::dispatch::{Dispatcher, EventSink};
use crate::protocol::{Command, Response};

pub(crate) struct Lifecycle {
  endpoint: String,
  pub(crate) generation: String,
  close_requests: std::sync::atomic::AtomicUsize,
  cleaned: std::sync::atomic::AtomicBool,
  dispatcher: Arc<dyn Dispatcher>,
  closing: watch::Sender<bool>,
  draining: watch::Sender<bool>,
  pub(crate) stopped: watch::Sender<bool>,
  runs: RwLock<()>,
  cleanup: Mutex<()>,
  publication: std::sync::Mutex<Option<crate::registry::RegistryClaim>>,
}

impl Lifecycle {
  pub(crate) fn new(endpoint: String, dispatcher: Arc<dyn Dispatcher>) -> crate::Result<Self> {
    use base64::Engine as _;
    let mut generation = [0; 32];
    getrandom::fill(&mut generation)
      .map_err(|error| crate::SessionError::Dispatch(format!("session identity generation failed: {error}")))?;
    Ok(Self {
      generation: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(generation),
      close_requests: std::sync::atomic::AtomicUsize::new(0),
      cleaned: std::sync::atomic::AtomicBool::new(false),
      endpoint,
      dispatcher,
      closing: watch::channel(false).0,
      draining: watch::channel(false).0,
      stopped: watch::channel(false).0,
      runs: RwLock::new(()),
      cleanup: Mutex::new(()),
      publication: std::sync::Mutex::new(None),
    })
  }

  pub(crate) fn close_request(self: &Arc<Self>) -> CloseRequest {
    self.close_requests.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    CloseRequest(Arc::clone(self))
  }

  pub(crate) fn publish(&self, claim: crate::registry::RegistryClaim) {
    *self
      .publication
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(claim);
  }

  pub(crate) fn unpublish(&self) -> crate::Result<()> {
    let mut publication = self
      .publication
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(claim) = publication.as_mut() {
      claim.remove()?;
    }
    publication.take();
    Ok(())
  }

  pub(crate) fn closing(&self) -> watch::Receiver<bool> {
    self.closing.subscribe()
  }

  pub(crate) fn draining(&self) -> watch::Receiver<bool> {
    self.draining.subscribe()
  }

  pub(crate) fn drain(&self) {
    self.draining.send_replace(true);
  }

  pub(crate) async fn run(&self, command: Command, events: EventSink) -> Response {
    let id = command.id;
    let mut closing = self.closing.subscribe();
    tokio::select! {
      biased;
      _ = closing.wait_for(|closing| *closing) => Response::err(id, "session is closing; retry close if cleanup failed"),
      response = async {
        let _run = self.runs.read().await;
        self.dispatcher.dispatch(command, events).await
      } => response,
    }
  }

  pub(crate) fn identifies(&self, command: &Command) -> bool {
    command.context.is_none()
      && command.args.get("endpoint").and_then(serde_json::Value::as_str) == Some(&self.endpoint)
      && command.args.get("generation").and_then(serde_json::Value::as_str) == Some(&self.generation)
  }

  pub(crate) async fn close(&self, command: &Command) -> Response {
    let _cleanup = self.cleanup.lock().await;
    self.closing.send_replace(true);
    let _runs = self.runs.write().await;
    match self.dispatcher.close().await {
      Ok(()) => match self.unpublish() {
        Ok(()) => {
          self.cleaned.store(true, std::sync::atomic::Ordering::Release);
          Response::ok(command.id, "session closed")
        },
        Err(error) => Response::err(command.id, error.to_string()),
      },
      Err(error) => Response::err(command.id, error),
    }
  }
}

pub(crate) struct CloseRequest(Arc<Lifecycle>);

impl Drop for CloseRequest {
  fn drop(&mut self) {
    if self.0.close_requests.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1
      && self.0.cleaned.load(std::sync::atomic::Ordering::Acquire)
    {
      self.0.stopped.send_replace(true);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::dispatch::test_support::EchoDispatcher;

  #[test]
  fn shutdown_waits_for_every_started_close_acknowledgement() {
    let lifecycle = Arc::new(Lifecycle::new("local".into(), Arc::new(EchoDispatcher)).unwrap());
    let first = lifecycle.close_request();
    let second = lifecycle.close_request();
    lifecycle.cleaned.store(true, std::sync::atomic::Ordering::Release);
    drop(first);
    assert!(!*lifecycle.stopped.borrow());
    drop(second);
    assert!(*lifecycle.stopped.borrow());
  }
}
