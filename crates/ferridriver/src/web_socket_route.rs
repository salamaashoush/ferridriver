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
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{Value, json};

use crate::backend::AnyPage;
use crate::error::Result;
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
        .unwrap_or_default();
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
  on_page_message: Option<WsMsgCb>,
  on_page_close: Option<WsCloseCb>,
  on_server_message: Option<WsMsgCb>,
  on_server_close: Option<WsCloseCb>,
}

struct WsRouteState {
  id: String,
  url: String,
  protocols: Vec<String>,
  page: AnyPage,
  callbacks: Mutex<WsCallbacks>,
  connected: AtomicBool,
}

impl WsRouteState {
  async fn dispatch(&self, request: Value) {
    let expr = format!(
      "globalThis.__pwWebSocketDispatch && globalThis.__pwWebSocketDispatch({})",
      serde_json::to_string(&request).unwrap_or_else(|_| "null".to_string())
    );
    let _ = self.page.evaluate(&expr).await;
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
  fn new(id: String, url: String, protocols: Vec<String>, page: AnyPage) -> Self {
    Self {
      inner: Arc::new(WsRouteState {
        id,
        url,
        protocols,
        page,
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
    self.lock().on_page_message = Some(cb);
  }

  /// Register a page-close handler. Playwright: `webSocketRoute.onClose`.
  pub fn on_close(&self, cb: WsCloseCb) {
    self.lock().on_page_close = Some(cb);
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
    let cb = self.lock().on_page_message.clone();
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
    let cb = self.lock().on_server_message.clone();
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
    let cb = self.lock().on_page_close.clone();
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
    let cb = self.lock().on_server_close.clone();
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
      .on_server_message = Some(cb);
  }

  /// Register a server-close handler. Playwright: server `onClose`.
  pub fn on_close(&self, cb: WsCloseCb) {
    self
      .inner
      .callbacks
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .on_server_close = Some(cb);
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
    Arc::new(Self {
      page,
      routes: Mutex::new(Vec::new()),
      active: Mutex::new(rustc_hash::FxHashMap::default()),
      installed: AtomicBool::new(false),
    })
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
  pub async fn handle_binding(self: &Arc<Self>, payload: &Value) {
    let kind = payload.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "onCreate" {
      self.handle_create(payload).await;
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

  async fn handle_create(self: &Arc<Self>, payload: &Value) {
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
      // No route matched — let the mock connect straight through.
      let req = json!({ "id": id, "type": "passthrough" });
      let expr = format!(
        "globalThis.__pwWebSocketDispatch && globalThis.__pwWebSocketDispatch({})",
        serde_json::to_string(&req).unwrap_or_else(|_| "null".to_string())
      );
      let _ = self.page.evaluate(&expr).await;
      return;
    };

    let route = WebSocketRoute::new(id.clone(), url, protocols, self.page.clone());
    self
      .active
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(id, route.clone());
    handler(route.clone()).await;
    route.after_handle().await;
  }
}

fn close_code(payload: &Value) -> Option<u32> {
  payload.get("code").and_then(Value::as_u64).map(|c| c as u32)
}

fn close_reason(payload: &Value) -> Option<String> {
  payload.get("reason").and_then(Value::as_str).map(String::from)
}

fn was_clean(payload: &Value) -> bool {
  payload.get("wasClean").and_then(Value::as_bool).unwrap_or(false)
}

/// Build the [`crate::events::ExposedFn`] that backs `__pwWebSocketBinding`
/// for a page router. Spreads the single payload arg into `handle_binding`.
#[must_use]
pub fn binding_callback(router: Arc<PageWsRouter>) -> crate::events::ExposedFn {
  Arc::new(move |args: Vec<Value>| {
    let router = router.clone();
    Box::pin(async move {
      if let Some(payload) = args.into_iter().next() {
        router.handle_binding(&payload).await;
      }
      Value::Null
    })
  })
}

/// The init-script source that installs the WebSocket mock.
#[must_use]
pub fn mock_init_script() -> Result<crate::options::InitScriptSource> {
  Ok(crate::options::InitScriptSource::Source(WS_MOCK_SOURCE.to_string()))
}
