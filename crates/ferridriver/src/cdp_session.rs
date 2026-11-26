//! `CDPSession` — raw Chrome `DevTools` Protocol access.
//!
//! Playwright: `browser.newBrowserCDPSession()` and
//! `browserContext.newCDPSession(page)` return a `CDPSession` with
//! `send(method, params?)`, `detach()`, and per-protocol-event listeners
//! (`client/cdpSession.ts`). Chromium-only — the `WebKit` and `BiDi`
//! backends return a typed [`FerriError::Unsupported`].
//!
//! Session creation mirrors `crConnection.ts`: a page session attaches
//! via root `Target.attachToTarget { targetId, flatten: true }`
//! (`crConnection.ts:236`), a browser session via
//! `Target.attachToBrowserTarget` (`crConnection.ts:101`) — both yield a
//! dedicated `sessionId` so user traffic never rides the session that
//! drives the page. Events are delivered over a lossless, wire-ordered
//! wildcard tap scoped strictly to that session id.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustc_hash::FxHashMap;

use crate::backend::cdp::transport::CdpTransport;
use crate::error::{FerriError, Result};

/// Callback invoked with a protocol event's `params` object.
pub type CdpEventCallback = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// Identifier returned by [`CdpSession::on`] for [`CdpSession::off`].
pub type CdpListenerId = u64;

struct Listener {
  id: CdpListenerId,
  callback: CdpEventCallback,
  once: bool,
}

#[derive(Default)]
struct ListenerRegistry {
  /// Exact protocol method (`"Network.requestWillBeSent"`) → listeners.
  by_method: FxHashMap<String, Vec<Listener>>,
  /// Listeners for every event (Playwright's `'event'` wildcard —
  /// invoked with `{ method, params }`).
  wildcard: Vec<Listener>,
}

struct SessionInner {
  transport: Arc<CdpTransport>,
  session_id: Arc<str>,
  detached: AtomicBool,
  listeners: std::sync::Mutex<ListenerRegistry>,
  next_listener_id: AtomicU64,
  pump_started: AtomicBool,
  /// Requests that the pump run every event already queued, then answer.
  flushes: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<tokio::sync::oneshot::Sender<()>>>,
  /// Captured at attach time (always inside the driver's tokio
  /// runtime) so listener registration can spawn the event pump from
  /// ANY thread — NAPI `session.on(...)` runs on the Node main thread,
  /// where `tokio::spawn` would panic.
  runtime: tokio::runtime::Handle,
}

/// A raw CDP session attached to a page target or the browser target.
#[derive(Clone)]
pub struct CdpSession {
  inner: Arc<SessionInner>,
}

impl CdpSession {
  pub(crate) async fn attach_to_target(transport: Arc<CdpTransport>, target_id: &str) -> Result<Self> {
    let result = transport
      .send(
        None,
        &ferridriver_cdp_proto::target::AttachToTarget {
          target_id,
          flatten: Some(true),
        },
      )
      .await?;
    Ok(Self::from_session_id(transport, result.session_id))
  }

  pub(crate) async fn attach_to_browser_target(transport: Arc<CdpTransport>) -> Result<Self> {
    let result = transport
      .send(None, &ferridriver_cdp_proto::target::AttachToBrowserTarget {})
      .await?;
    Ok(Self::from_session_id(transport, result.session_id))
  }

  fn from_session_id(transport: Arc<CdpTransport>, session_id: String) -> Self {
    Self {
      inner: Arc::new(SessionInner {
        transport,
        session_id: Arc::from(session_id),
        detached: AtomicBool::new(false),
        listeners: std::sync::Mutex::new(ListenerRegistry::default()),
        next_listener_id: AtomicU64::new(1),
        pump_started: AtomicBool::new(false),
        flushes: std::sync::OnceLock::new(),
        runtime: tokio::runtime::Handle::current(),
      }),
    }
  }

  /// Send a raw protocol command on this session. Playwright:
  /// `cdpSession.send(method, params?)`.
  ///
  /// # Errors
  ///
  /// Errors if the session was detached or the protocol reports an error.
  pub async fn send(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    if self.inner.detached.load(Ordering::SeqCst) {
      return Err(FerriError::target_closed(Some(
        "Session already detached. Most likely the page has been closed.".to_string(),
      )));
    }
    self
      .inner
      .transport
      .send_command(Some(&self.inner.session_id), method, &params)
      .await
  }

  /// Detach the session. Playwright: `cdpSession.detach()` —
  /// `Runtime.runIfWaitingForDebugger` first (backend quirk,
  /// `crConnection.ts:181-184`), then root
  /// `Target.detachFromTarget { sessionId }`.
  ///
  /// # Errors
  ///
  /// Errors if the session was already detached or the protocol call fails.
  pub async fn detach(&self) -> Result<()> {
    if self.inner.detached.swap(true, Ordering::SeqCst) {
      return Err(FerriError::target_closed(Some(
        "Session already detached. Most likely the page has been closed.".to_string(),
      )));
    }
    let _ = self
      .inner
      .transport
      .send(
        Some(&self.inner.session_id),
        &ferridriver_cdp_proto::runtime::RunIfWaitingForDebugger {},
      )
      .await;
    self
      .inner
      .transport
      .send(
        None,
        &ferridriver_cdp_proto::target::DetachFromTarget {
          session_id: Some(&self.inner.session_id),
          target_id: None,
        },
      )
      .await?;
    Ok(())
  }

  /// Register a listener for one protocol event method. The callback
  /// receives the event's `params` object. Mirrors
  /// `cdpSession.on('Network.requestWillBeSent', params => ...)`.
  pub fn on(&self, method: &str, callback: CdpEventCallback) -> CdpListenerId {
    self.register(Some(method), callback, false)
  }

  /// One-shot variant of [`Self::on`].
  pub fn once(&self, method: &str, callback: CdpEventCallback) -> CdpListenerId {
    self.register(Some(method), callback, true)
  }

  /// Listener for EVERY protocol event on this session. The callback
  /// receives the full `{ method, params }` envelope (Playwright's
  /// `'event'` wildcard, `client/cdpSession.ts:33`).
  pub fn on_any(&self, callback: CdpEventCallback) -> CdpListenerId {
    self.register(None, callback, false)
  }

  /// One-shot variant of [`Self::on_any`].
  pub fn once_any(&self, callback: CdpEventCallback) -> CdpListenerId {
    self.register(None, callback, true)
  }

  /// Remove a previously registered listener.
  pub fn off(&self, id: CdpListenerId) {
    let mut registry = self
      .inner
      .listeners
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.wildcard.retain(|l| l.id != id);
    for listeners in registry.by_method.values_mut() {
      listeners.retain(|l| l.id != id);
    }
  }

  fn register(&self, method: Option<&str>, callback: CdpEventCallback, once: bool) -> CdpListenerId {
    let id = self.inner.next_listener_id.fetch_add(1, Ordering::Relaxed);
    {
      let mut registry = self
        .inner
        .listeners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let listener = Listener { id, callback, once };
      match method {
        Some(m) => registry.by_method.entry(m.to_string()).or_default().push(listener),
        None => registry.wildcard.push(listener),
      }
    }
    self.ensure_pump();
    id
  }

  /// Wait until every event that reached this session before the call
  /// has been handed to its listeners. A command's reply arrives after
  /// the events sent ahead of it, so after awaiting one this sees them all.
  pub async fn flush(&self) {
    let Some(flushes) = self.inner.flushes.get() else {
      return;
    };
    let (done, drained) = tokio::sync::oneshot::channel();
    if flushes.send(done).is_ok() {
      let _ = drained.await;
    }
  }

  /// Spawn the event pump on first listener registration: drains the
  /// lossless session tap and dispatches to registered callbacks.
  fn ensure_pump(&self) {
    if self.inner.pump_started.swap(true, Ordering::SeqCst) {
      return;
    }
    let mut rx = self.inner.transport.tap_all_events(&self.inner.session_id);
    let (flush_tx, mut flushes) = tokio::sync::mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<()>>();
    let _ = self.inner.flushes.set(flush_tx);
    let inner = Arc::downgrade(&self.inner);
    self.inner.runtime.spawn(async move {
      loop {
        tokio::select! {
          biased;
          Some(done) = flushes.recv() => {
            while let Ok(event) = rx.try_recv() {
              let Some(inner) = inner.upgrade() else { return };
              dispatch(&inner, &event);
            }
            let _ = done.send(());
          },
          event = rx.recv() => {
            let Some(event) = event else { break };
            let Some(inner) = inner.upgrade() else { break };
            dispatch(&inner, &event);
          },
        }
      }
    });
  }
}

/// Hand one session event to the listeners registered for it: method
/// listeners get `params`, wildcard listeners the `{ method, params }`
/// envelope (Playwright's `'event'`).
fn dispatch(inner: &SessionInner, event: &serde_json::Value) {
  let Some(method) = event.get("method").and_then(serde_json::Value::as_str) else {
    return;
  };
  let params = event.get("params").cloned().unwrap_or(serde_json::Value::Null);
  let (method_cbs, wildcard_cbs) = {
    let mut registry = inner
      .listeners
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let method_cbs: Vec<CdpEventCallback> = registry
      .by_method
      .get(method)
      .map(|ls| ls.iter().map(|l| Arc::clone(&l.callback)).collect())
      .unwrap_or_default();
    if let Some(listeners) = registry.by_method.get_mut(method) {
      listeners.retain(|l| !l.once);
    }
    let wildcard_cbs: Vec<CdpEventCallback> = registry.wildcard.iter().map(|l| Arc::clone(&l.callback)).collect();
    registry.wildcard.retain(|l| !l.once);
    (method_cbs, wildcard_cbs)
  };
  for cb in method_cbs {
    cb(params.clone());
  }
  for cb in wildcard_cbs {
    cb(serde_json::json!({ "method": method, "params": params.clone() }));
  }
}
