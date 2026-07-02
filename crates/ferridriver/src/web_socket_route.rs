//! `WebSocketRoute` — page-side WebSocket interception
//! (`page.routeWebSocket` / `context.routeWebSocket`, Playwright 1.60).
//!
//! WS interception is done in-page, not at the protocol level: an init
//! script (`injected/dist/websocket-mock.min.js`) overrides
//! `globalThis.WebSocket` with a mock that proxies every WebSocket
//! through the exposed `__pwWebSocketBinding` function (page→driver) and
//! the `globalThis.__pwWebSocketDispatch` hook (driver→page). This
//! module is the driver-side counterpart of Playwright's
//! `WebSocketRouteDispatcher` (`server/dispatchers/webSocketRouteDispatcher.ts`)
//! plus the default-forwarding logic from the client `WebSocketRoute`
//! (`client/network.ts`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use base64::Engine;
use serde_json::{Value, json};

use crate::backend::AnyPage;
use crate::url_matcher::UrlMatcher;

/// Binding name the mock calls to notify the driver of WS events. Must
/// match `webSocketMock.ts` (`__pwWebSocketBinding`).
pub const WS_BINDING_NAME: &str = "__pwWebSocketBinding";

/// Bundled WebSocket-mock init script (overrides `globalThis.WebSocket`).
pub const WS_MOCK_SOURCE: &str = include_str!("injected/dist/websocket-mock.min.js");

/// A WebSocket frame payload — text or binary.
#[derive(Clone, Debug)]
pub enum WsMessage {
  Text(String),
  Binary(Vec<u8>),
}

impl WsMessage {
  fn to_wsdata(&self) -> Value {
    match self {
      WsMessage::Text(s) => json!({ "data": s, "isBase64": false }),
      WsMessage::Binary(b) => json!({
        "data": base64::engine::general_purpose::STANDARD.encode(b),
        "isBase64": true,
      }),
    }
  }

  fn from_wsdata(data: &Value) -> Self {
    let is_base64 = data.get("isBase64").and_then(Value::as_bool).unwrap_or(false);
    let raw = data.get("data").and_then(Value::as_str).unwrap_or("");
    if is_base64 {
      let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .unwrap_or_else(|e| {
          tracing::warn!(error = %e, "WS binary frame: malformed base64 from page mock; treating as empty");
          Vec::new()
        });
      WsMessage::Binary(bytes)
    } else {
      WsMessage::Text(raw.to_string())
    }
  }
}

/// Future returned by a [`WsHandler`] — resolves once the handler's
/// synchronous setup (onMessage / connectToServer / etc.) has run.
pub type WsHandlerFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Handler invoked when a WebSocket matches a registered route. Receives
/// the live [`WebSocketRoute`]. Async so the driver can await the
/// handler's setup before deciding whether to connect upstream or open a
/// fully-mocked socket (mirrors Playwright awaiting the route handler).
pub type WsHandler = Arc<dyn Fn(WebSocketRoute) -> WsHandlerFuture + Send + Sync>;
type WsMsgCb = Arc<dyn Fn(WsMessage) + Send + Sync>;
type WsCloseCb = Arc<dyn Fn(Option<u32>, Option<String>) + Send + Sync>;

#[derive(Default)]
struct WsCallbacks {
  page_message: Option<WsMsgCb>,
  page_close: Option<WsCloseCb>,
  server_message: Option<WsMsgCb>,
  server_close: Option<WsCloseCb>,
}

struct WsRouteState {
  id: String,
  url: String,
  protocols: Vec<String>,
  page: AnyPage,
  /// Frame the socket was created in (from the binding call's
  /// `BindingSource`); `None` means the backend could not resolve the
  /// calling frame and dispatch falls back to the main frame. Mirrors
  /// Playwright's `WebSocketRouteDispatcher._frame`.
  frame_id: Option<String>,
  callbacks: Mutex<WsCallbacks>,
  connected: AtomicBool,
}

impl WsRouteState {
  async fn dispatch(&self, request: Value) {
    // Drive the page-side mock through the SAME main-world-anchored eval
    // path the socket was created on (`call_utility_evaluate` in the
    // socket's own frame — an iframe socket has its own mock +
    // `idToWebSocket` map), not a bare `Runtime.evaluate`. On WebKit a
    // bare evaluate targets the target's ambiguous default context,
    // which — during the transient dual-context window right after a
    // cross-process navigation commit — can differ from the context that
    // holds the mock's `idToWebSocket` map, silently dropping the dispatch.
    // Anchoring on the frame's main world (as Playwright's
    // `frame.evaluateExpression` does) keeps the dispatch and the socket
    // in the same realm.
    let fn_source = format!(
      "() => {{ globalThis.__pwWebSocketDispatch && globalThis.__pwWebSocketDispatch({}); }}",
      serde_json::to_string(&request).unwrap_or_else(|_| "null".to_string())
    );
    let _ = self
      .page
      .call_utility_evaluate(&fn_source, &[], &[], self.frame_id.as_deref(), Some(true), true)
      .await;
  }
}

/// Page-side route handle — Playwright's `WebSocketRoute`. Controls the
/// page's view of the socket (`send` → page, `close` → page) and, via
/// [`Self::connect_to_server`], the upstream connection.
#[derive(Clone)]
pub struct WebSocketRoute {
  inner: Arc<WsRouteState>,
}

impl WebSocketRoute {
  fn new(id: String, url: String, protocols: Vec<String>, page: AnyPage, frame_id: Option<String>) -> Self {
    Self {
      inner: Arc::new(WsRouteState {
        id,
        url,
        protocols,
        page,
        frame_id,
        callbacks: Mutex::new(WsCallbacks::default()),
        connected: AtomicBool::new(false),
      }),
    }
  }

  /// Playwright: `webSocketRoute.url()`.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }

  /// Playwright: `webSocketRoute.protocols()`.
  #[must_use]
  pub fn protocols(&self) -> &[String] {
    &self.inner.protocols
  }

  /// Send a message to the page (as if from the server). Playwright:
  /// `webSocketRoute.send(message)`.
  pub async fn send(&self, message: WsMessage) {
    self
      .inner
      .dispatch(json!({ "id": self.inner.id, "type": "sendToPage", "data": message.to_wsdata() }))
      .await;
  }

  /// Close the page side of the socket. Playwright:
  /// `webSocketRoute.close({ code?, reason? })`.
  pub async fn close(&self, code: Option<u32>, reason: Option<String>) {
    self
      .inner
      .dispatch(json!({
        "id": self.inner.id, "type": "closePage",
        "code": code, "reason": reason, "wasClean": true,
      }))
      .await;
  }

  /// Register a page-message handler. Playwright:
  /// `webSocketRoute.onMessage(handler)`. When set, page messages are
  /// delivered here instead of auto-forwarded to the server.
  pub fn on_message(&self, cb: WsMsgCb) {
    self.lock().page_message = Some(cb);
  }

  /// Register a page-close handler. Playwright: `webSocketRoute.onClose`.
  pub fn on_close(&self, cb: WsCloseCb) {
    self.lock().page_close = Some(cb);
  }

  /// Connect to the real upstream server and return the server-side
  /// handle. Playwright: `webSocketRoute.connectToServer()`. Synchronous:
  /// it records the intent (so the driver, after the handler runs,
  /// dispatches the actual `connect` to the page mock).
  #[must_use]
  pub fn connect_to_server(&self) -> WebSocketRouteServer {
    self.inner.connected.store(true, Ordering::SeqCst);
    WebSocketRouteServer {
      inner: self.inner.clone(),
    }
  }

  /// Whether [`Self::connect_to_server`] has been called.
  #[must_use]
  pub fn is_connected(&self) -> bool {
    self.inner.connected.load(Ordering::SeqCst)
  }

  /// Called after the route handler resolves. If the handler connected
  /// to a server, drive the mock to open the real upstream socket;
  /// otherwise tell it to open a fully-mocked socket so the page can
  /// send/receive without a server. Mirrors the client
  /// `WebSocketRoute.connectToServer` + `_afterHandle`.
  async fn after_handle(&self) {
    let req = if self.is_connected() {
      json!({ "id": self.inner.id, "type": "connect" })
    } else {
      json!({ "id": self.inner.id, "type": "ensureOpened" })
    };
    self.inner.dispatch(req).await;
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, WsCallbacks> {
    self
      .inner
      .callbacks
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  // --- driver-side event delivery (called by the binding dispatcher) ---

  async fn on_message_from_page(&self, data: &Value) {
    let cb = self.lock().page_message.clone();
    if let Some(cb) = cb {
      cb(WsMessage::from_wsdata(data));
    } else if self.is_connected() {
      self
        .inner
        .dispatch(json!({ "id": self.inner.id, "type": "sendToServer", "data": data }))
        .await;
    }
  }

  async fn on_message_from_server(&self, data: &Value) {
    let cb = self.lock().server_message.clone();
    if let Some(cb) = cb {
      cb(WsMessage::from_wsdata(data));
    } else {
      self
        .inner
        .dispatch(json!({ "id": self.inner.id, "type": "sendToPage", "data": data }))
        .await;
    }
  }

  async fn on_close_page(&self, code: Option<u32>, reason: Option<String>, was_clean: bool) {
    let cb = self.lock().page_close.clone();
    if let Some(cb) = cb {
      cb(code, reason);
    } else {
      self
        .inner
        .dispatch(json!({
          "id": self.inner.id, "type": "closeServer",
          "code": code, "reason": reason, "wasClean": was_clean,
        }))
        .await;
    }
  }

  async fn on_close_server(&self, code: Option<u32>, reason: Option<String>, was_clean: bool) {
    let cb = self.lock().server_close.clone();
    if let Some(cb) = cb {
      cb(code, reason);
    } else {
      self
        .inner
        .dispatch(json!({
          "id": self.inner.id, "type": "closePage",
          "code": code, "reason": reason, "wasClean": was_clean,
        }))
        .await;
    }
  }

  /// The socket's frame navigated away, detached, or the page closed —
  /// no more communication from the page-side mock is possible, so
  /// pretend the socket closed cleanly on both sides. Mirrors
  /// Playwright's `WebSocketRouteDispatcher._executionContextGone`:
  /// only the user-facing close callbacks fire; the default
  /// forward-to-page dispatches are skipped because the frame's mock is
  /// gone (Playwright sends them into the dead frame and swallows the
  /// instant rejection — same observable effect). Synchronous so the
  /// router's lifecycle listener never blocks behind a teardown.
  fn execution_context_gone(&self) {
    let (page_cb, server_cb) = {
      let cbs = self.lock();
      (cbs.page_close.clone(), cbs.server_close.clone())
    };
    if let Some(cb) = page_cb {
      cb(None, None);
    }
    if let Some(cb) = server_cb {
      cb(None, None);
    }
  }
}

/// Server-side handle returned by [`WebSocketRoute::connect_to_server`].
/// Playwright's `webSocketRoute.connectToServer()` return value.
#[derive(Clone)]
pub struct WebSocketRouteServer {
  inner: Arc<WsRouteState>,
}

impl WebSocketRouteServer {
  /// Playwright: server-side `url()`.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }

  /// Send a message to the upstream server. Playwright: server `send`.
  pub async fn send(&self, message: WsMessage) {
    self
      .inner
      .dispatch(json!({ "id": self.inner.id, "type": "sendToServer", "data": message.to_wsdata() }))
      .await;
  }

  /// Close the upstream connection. Playwright: server `close`.
  pub async fn close(&self, code: Option<u32>, reason: Option<String>) {
    self
      .inner
      .dispatch(json!({
        "id": self.inner.id, "type": "closeServer",
        "code": code, "reason": reason, "wasClean": true,
      }))
      .await;
  }

  /// Register a server-message handler. Playwright: server `onMessage`.
  pub fn on_message(&self, cb: WsMsgCb) {
    self
      .inner
      .callbacks
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .server_message = Some(cb);
  }

  /// Register a server-close handler. Playwright: server `onClose`.
  pub fn on_close(&self, cb: WsCloseCb) {
    self
      .inner
      .callbacks
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .server_close = Some(cb);
  }
}

/// Per-page WebSocket-route registry. Holds the registered matchers +
/// handlers and the live `id -> WebSocketRoute` map. Shared (Arc) between
/// the owning page and the exposed-binding dispatcher closure.
pub struct PageWsRouter {
  page: AnyPage,
  routes: Mutex<Vec<(UrlMatcher, WsHandler)>>,
  active: Mutex<rustc_hash::FxHashMap<String, WebSocketRoute>>,
  installed: AtomicBool,
}

impl PageWsRouter {
  #[must_use]
  pub fn new(page: AnyPage) -> Arc<Self> {
    let router = Arc::new(Self {
      page,
      routes: Mutex::new(Vec::new()),
      active: Mutex::new(rustc_hash::FxHashMap::default()),
      installed: AtomicBool::new(false),
    });
    Self::spawn_lifecycle_listener(&router);
    router
  }

  /// Mirror Playwright's `WebSocketRouteDispatcher` frame listeners:
  /// when a route's frame navigates to a new document or detaches — or
  /// the page closes — the page-side mock is gone, so close the route's
  /// user-facing side cleanly and drop it from the active map. Holds a
  /// `Weak` so the listener never extends the router's life (the durable
  /// owner stays the exposed-binding closure). Every event is handled
  /// synchronously — a blocked listener would process a navigation event
  /// late and tear down a route the NEW document just created.
  fn spawn_lifecycle_listener(router: &Arc<Self>) {
    let mut rx = router.page.events().subscribe();
    let weak = Arc::downgrade(router);
    tokio::spawn(async move {
      while let Some(event) = crate::events::recv_tolerant(&mut rx).await {
        let Some(router) = weak.upgrade() else { break };
        match event {
          crate::events::PageEvent::FrameNavigated(info) => {
            router.frame_context_gone(&info.frame_id);
          },
          crate::events::PageEvent::FrameDetached { frame_id } => {
            router.frame_context_gone(&frame_id);
          },
          crate::events::PageEvent::Close => {
            router.all_contexts_gone();
            break;
          },
          _ => {},
        }
      }
    });
  }

  fn frame_context_gone(&self, frame_id: &str) {
    let gone: Vec<WebSocketRoute> = {
      let mut active = self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      let ids: Vec<String> = active
        .iter()
        .filter(|(_, r)| r.inner.frame_id.as_deref() == Some(frame_id))
        .map(|(id, _)| id.clone())
        .collect();
      ids.iter().filter_map(|id| active.remove(id)).collect()
    };
    for route in gone {
      route.execution_context_gone();
    }
  }

  fn all_contexts_gone(&self) {
    let gone: Vec<WebSocketRoute> = {
      let mut active = self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      active.drain().map(|(_, r)| r).collect()
    };
    for route in gone {
      route.execution_context_gone();
    }
  }

  /// Register a new route. Returns `true` the first time (caller must
  /// then install the binding + init script).
  pub fn add_route(&self, matcher: UrlMatcher, handler: WsHandler) -> bool {
    self
      .routes
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push((matcher, handler));
    !self.installed.swap(true, Ordering::SeqCst)
  }

  /// Handle one `__pwWebSocketBinding` payload from the page.
  /// `source_frame` is the calling frame from the binding's
  /// `BindingSource` — an iframe-created socket dispatches back into
  /// that same frame (Playwright stores `source.frame` at `onCreate`).
  pub async fn handle_binding(self: &Arc<Self>, payload: &Value, source_frame: Option<&str>) {
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "onCreate" {
      self.handle_create(payload, source_frame).await;
      return;
    }
    let Some(id) = payload.get("id").and_then(Value::as_str) else {
      return;
    };
    let route = self
      .active
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(id)
      .cloned();
    let Some(route) = route else { return };
    match kind {
      "onMessageFromPage" => {
        if let Some(data) = payload.get("data") {
          route.on_message_from_page(data).await;
        }
      },
      "onMessageFromServer" => {
        if let Some(data) = payload.get("data") {
          route.on_message_from_server(data).await;
        }
      },
      "onClosePage" => {
        route
          .on_close_page(close_code(payload), close_reason(payload), was_clean(payload))
          .await;
      },
      "onCloseServer" => {
        route
          .on_close_server(close_code(payload), close_reason(payload), was_clean(payload))
          .await;
      },
      _ => {},
    }
  }

  async fn handle_create(self: &Arc<Self>, payload: &Value, source_frame: Option<&str>) {
    let id = payload.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let url = payload.get("url").and_then(Value::as_str).unwrap_or("").to_string();
    let protocols = payload
      .get("protocols")
      .and_then(Value::as_array)
      .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
      .unwrap_or_default();

    let handler = {
      let routes = self.routes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      routes.iter().find(|(m, _)| m.matches(&url)).map(|(_, h)| h.clone())
    };

    let Some(handler) = handler else {
      // No route matched — let the mock connect straight through, in
      // the frame the socket was created in (Playwright:
      // `source.frame.evaluateExpression` on the passthrough path).
      let req = json!({ "id": id, "type": "passthrough" });
      let fn_source = format!(
        "() => {{ globalThis.__pwWebSocketDispatch && globalThis.__pwWebSocketDispatch({}); }}",
        serde_json::to_string(&req).unwrap_or_else(|_| "null".to_string())
      );
      let _ = self
        .page
        .call_utility_evaluate(&fn_source, &[], &[], source_frame, Some(true), true)
        .await;
      return;
    };

    let route = WebSocketRoute::new(
      id.clone(),
      url,
      protocols,
      self.page.clone(),
      source_frame.map(String::from),
    );
    self
      .active
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(id, route.clone());
    handler(route.clone()).await;
    route.after_handle().await;
  }
}

/// Live WS routers keyed by backend page id. Shared per backend page (not
/// per `Page` wrapper) so `page.routeWebSocket` and `context.routeWebSocket`
/// — which re-wraps its pages into fresh `Page` handles — install a single
/// `__pwWebSocketBinding` + mock init script instead of competing copies
/// that would overwrite each other's binding. The durable owner is the
/// exposed-binding closure ([`binding_callback`]); the map holds only a
/// `Weak`, so a closed page's entry upgrades to `None` and self-heals.
static WS_ROUTERS: OnceLock<Mutex<rustc_hash::FxHashMap<usize, Weak<PageWsRouter>>>> = OnceLock::new();

/// Resolve the shared [`PageWsRouter`] for `page_id`, creating one bound to
/// `page` if none is currently live. The caller installs the binding + init
/// script only when [`PageWsRouter::add_route`] reports it registered the
/// first route (which is exactly the first time a router is created here).
#[must_use]
pub fn router_for_page(page_id: usize, page: AnyPage) -> Arc<PageWsRouter> {
  let map = WS_ROUTERS.get_or_init(|| Mutex::new(rustc_hash::FxHashMap::default()));
  let mut guard = map.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  if let Some(existing) = guard.get(&page_id).and_then(Weak::upgrade) {
    return existing;
  }
  let router = PageWsRouter::new(page);
  guard.insert(page_id, Arc::downgrade(&router));
  router
}

fn close_code(payload: &Value) -> Option<u32> {
  payload
    .get("code")
    .and_then(Value::as_u64)
    .and_then(|c| u32::try_from(c).ok())
}

fn close_reason(payload: &Value) -> Option<String> {
  payload.get("reason").and_then(Value::as_str).map(String::from)
}

fn was_clean(payload: &Value) -> bool {
  payload.get("wasClean").and_then(Value::as_bool).unwrap_or(false)
}

/// Build the [`crate::events::ExposedBinding`] that backs
/// `__pwWebSocketBinding` for a page router. Spreads the single payload
/// arg into `handle_binding`, threading the calling frame from the
/// `BindingSource` so iframe-created sockets dispatch back into their
/// own frame.
#[must_use]
pub fn binding_callback(router: Arc<PageWsRouter>) -> crate::events::ExposedBinding {
  Arc::new(move |source: crate::events::BindingSource, args: Vec<Value>| {
    let router = router.clone();
    Box::pin(async move {
      if let Some(payload) = args.into_iter().next() {
        let frame = (!source.frame.is_empty()).then_some(source.frame.as_str());
        router.handle_binding(&payload, frame).await;
      }
      Value::Null
    })
  })
}

/// The init-script source that installs the WebSocket mock.
#[must_use]
pub fn mock_init_script() -> crate::options::InitScriptSource {
  crate::options::InitScriptSource::Source(WS_MOCK_SOURCE.to_string())
}
