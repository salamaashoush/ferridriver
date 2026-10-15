//! A page listener hears everything that arrives once it has been
//! started, including what arrives before its task is first polled.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::transport::{CdpDispatcher, CdpTransport, FrameStateObserver, IframeTargetSubscription};
use super::{CdpPage, LifecycleState};
use crate::error::Result;

/// The dispatcher with no browser behind it: events are whatever a test
/// dispatches, and a command is answered with nothing.
struct Wire {
  dispatcher: CdpDispatcher,
}

impl CdpTransport for Wire {
  fn close(&self) -> impl Future<Output = Result<()>> {
    std::future::ready(Ok(()))
  }

  fn is_disconnected(&self) -> bool {
    self.dispatcher.is_disconnected()
  }

  fn closed(&self) -> futures::future::BoxFuture<'static, ()> {
    Box::pin(std::future::pending())
  }

  fn send_command(
    &self,
    _session_id: Option<&str>,
    _method: &str,
    _params: &serde_json::Value,
  ) -> impl Future<Output = Result<serde_json::Value>> {
    std::future::ready(Ok(serde_json::Value::Null))
  }

  fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<Arc<serde_json::Value>> {
    self.dispatcher.subscribe_events()
  }

  fn subscribe_event_method(&self, method: &'static str) -> tokio::sync::broadcast::Receiver<Arc<serde_json::Value>> {
    self.dispatcher.subscribe_event_method(method)
  }

  fn subscribe_event_domain(&self, domain: &'static str) -> tokio::sync::broadcast::Receiver<Arc<serde_json::Value>> {
    self.dispatcher.subscribe_event_domain(domain)
  }

  fn tap_event_methods(
    &self,
    methods: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    self.dispatcher.tap_event_methods(methods, session_id)
  }

  fn tap_event_domains(
    &self,
    domains: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    self.dispatcher.tap_event_domains(domains, session_id)
  }

  fn tap_all_events(&self, session_id: &str) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    self.dispatcher.tap_all_events(session_id)
  }

  fn tap_iframe_targets(&self) -> IframeTargetSubscription {
    self.dispatcher.tap_iframe_targets()
  }

  fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
    frame_observer: FrameStateObserver,
  ) {
    self
      .dispatcher
      .register_lifecycle_tracker(session_id, state, notify, frame_observer);
  }

  fn unregister_session(&self, session_id: &str) {
    self.dispatcher.unregister_session(session_id);
  }
}

// The listeners are functions of a page type that can be wrapped; none of
// them wraps anything, and there is no `AnyPage` for a wire with no browser.
impl super::CdpWrap for Wire {
  fn wrap_page(_page: CdpPage<Self>) -> crate::backend::AnyPage {
    unreachable!("a listener wraps no page")
  }

  fn wrap_element(_element: super::CdpElement<Self>) -> crate::backend::AnyElement {
    unreachable!("a listener wraps no element")
  }
}

#[tokio::test]
async fn a_dialog_that_opens_before_the_listener_is_first_polled_is_delivered() {
  let wire = Arc::new(Wire {
    dispatcher: CdpDispatcher::new(),
  });
  let manager = crate::dialog::DialogManager::new();
  let opened = Arc::new(AtomicUsize::new(0));
  let counted = Arc::clone(&opened);
  manager.add_handler(Arc::new(move |dialog| {
    assert_eq!(dialog.message(), "boolean?");
    counted.fetch_add(1, Ordering::SeqCst);
    true
  }));

  let listener = CdpPage::<Wire>::spawn_dialog_listener(
    Arc::clone(&wire),
    Some(Arc::from("session")),
    Arc::default(),
    crate::events::EventEmitter::new(),
    manager,
    crate::backend::PageBackref::new(),
  );
  // No await since the spawn: on this single-threaded runtime the
  // listener's task has not run yet, which is the moment a loaded
  // runtime leaves open between starting a page and its first dialog.
  wire.dispatcher.dispatch_message(
    br#"{"sessionId":"session","method":"Page.javascriptDialogOpening","params":{"type":"confirm","message":"boolean?","defaultPrompt":""}}"#,
  );

  tokio::time::timeout(std::time::Duration::from_secs(5), async {
    while opened.load(Ordering::SeqCst) == 0 {
      tokio::task::yield_now().await;
    }
  })
  .await
  .expect("the dialog reached its handler");
  assert_eq!(opened.load(Ordering::SeqCst), 1);
  listener.abort();
}
