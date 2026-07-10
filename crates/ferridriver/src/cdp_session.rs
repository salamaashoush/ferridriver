//! `CDPSession` — raw Chrome DevTools Protocol access.
//!
//! Playwright: `browser.newBrowserCDPSession()` and
//! `browserContext.newCDPSession(page)` return a `CDPSession` with
//! `send(method, params?)`, `detach()`, and per-protocol-event listeners
//! (`client/cdpSession.ts`). Chromium-only — the WebKit and BiDi
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

use crate::backend::cdp::pipe::PipeTransport;
use crate::backend::cdp::transport::CdpTransport;
use crate::backend::cdp::ws::WsTransport;
use crate::error::{FerriError, Result};

/// Type-erased sender over the two concrete CDP transports.
/// `CdpTransport::send_command` is RPITIT (not dyn-compatible), so the
/// erasure is a two-variant enum — the same idiom as `AnyPage`.
#[derive(Clone)]
enum SessionTransport {
  Pipe(Arc<PipeTransport>),
  Ws(Arc<WsTransport>),
}

impl SessionTransport {
  async fn send(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<serde_json::Value> {
    match self {
      Self::Pipe(t) => t.send_command(session_id, method, params).await,
      Self::Ws(t) => t.send_command(session_id, method, params).await,
    }
  }

  fn tap_all(&self, session_id: &str) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    match self {
      Self::Pipe(t) => t.tap_all_events(session_id),
      Self::Ws(t) => t.tap_all_events(session_id),
    }
  }
}

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
  transport: SessionTransport,
  session_id: Arc<str>,
  detached: AtomicBool,
  listeners: std::sync::Mutex<ListenerRegistry>,
  next_listener_id: AtomicU64,
  pump_started: AtomicBool,
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
  pub(crate) async fn attach_to_target(transport: SessionTransportSource, target_id: &str) -> Result<Self> {
    let transport = transport.erase();
    let result = transport
      .send(
        None,
        "Target.attachToTarget",
        &serde_json::json!({ "targetId": target_id, "flatten": true }),
      )
      .await?;
    Self::from_attach_result(transport, &result)
  }

  pub(crate) async fn attach_to_browser_target(transport: SessionTransportSource) -> Result<Self> {
    let transport = transport.erase();
    let result = transport
      .send(None, "Target.attachToBrowserTarget", &serde_json::json!({}))
      .await?;
    Self::from_attach_result(transport, &result)
  }

  fn from_attach_result(transport: SessionTransport, result: &serde_json::Value) -> Result<Self> {
    let session_id = result
      .get("sessionId")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("Target.attachToTarget", "missing sessionId"))?;
    Ok(Self {
      inner: Arc::new(SessionInner {
        transport,
        session_id: Arc::from(session_id),
        detached: AtomicBool::new(false),
        listeners: std::sync::Mutex::new(ListenerRegistry::default()),
        next_listener_id: AtomicU64::new(1),
        pump_started: AtomicBool::new(false),
        runtime: tokio::runtime::Handle::current(),
      }),
    })
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
      .send(Some(&self.inner.session_id), method, &params)
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
        "Runtime.runIfWaitingForDebugger",
        &serde_json::json!({}),
      )
      .await;
    self
      .inner
      .transport
      .send(
        None,
        "Target.detachFromTarget",
        &serde_json::json!({ "sessionId": &*self.inner.session_id }),
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

  /// Spawn the event pump on first listener registration: drains the
  /// lossless session tap and dispatches to registered callbacks.
  fn ensure_pump(&self) {
    if self.inner.pump_started.swap(true, Ordering::SeqCst) {
      return;
    }
    let mut rx = self.inner.transport.tap_all(&self.inner.session_id);
    let inner = Arc::downgrade(&self.inner);
    self.inner.runtime.spawn(async move {
      while let Some(event) = rx.recv().await {
        let Some(inner) = inner.upgrade() else { break };
        let Some(method) = event.get("method").and_then(serde_json::Value::as_str) else {
          continue;
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
        // Method listeners get `params`; wildcard listeners get the
        // `{ method, params }` envelope (Playwright's `'event'`).
        for cb in method_cbs {
          cb(params.clone());
        }
        for cb in wildcard_cbs {
          cb(serde_json::json!({ "method": method, "params": params.clone() }));
        }
      }
    });
  }
}

/// Source for building the type-erased transport — constructed by the
/// CDP backend variants that own a concrete transport.
pub(crate) enum SessionTransportSource {
  Pipe(Arc<PipeTransport>),
  Ws(Arc<WsTransport>),
}

impl SessionTransportSource {
  fn erase(self) -> SessionTransport {
    match self {
      Self::Pipe(t) => SessionTransport::Pipe(t),
      Self::Ws(t) => SessionTransport::Ws(t),
    }
  }
}
