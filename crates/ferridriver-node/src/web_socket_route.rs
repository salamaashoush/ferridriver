//! NAPI bindings for `WebSocketRoute` / its server-side handle.
//!
//! Mirrors Playwright's `WebSocketRoute` (`client/network.ts`). The
//! handler passed to `page.routeWebSocket` receives a [`WebSocketRoute`];
//! `connectToServer()` returns a [`WebSocketRouteServer`].

use std::sync::Arc;

use ferridriver::web_socket_route::{WebSocketRoute as CoreRoute, WebSocketRouteServer as CoreServer, WsMessage};
use napi::bindgen_prelude::{Buffer, Either, Function, ToNapiValue};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi_derive::napi;

/// Options for `close({ code?, reason? })`.
#[napi(object)]
pub struct WebSocketCloseOptions {
  pub code: Option<u32>,
  pub reason: Option<String>,
}

/// Cross-thread arg delivering a WS message to a JS handler as
/// `string | Buffer` (text → string, binary → Buffer).
pub struct WsMessageArg(WsMessage);

impl ToNapiValue for WsMessageArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    match val.0 {
      WsMessage::Text(s) => unsafe { String::to_napi_value(env, s) },
      WsMessage::Binary(b) => unsafe { Buffer::to_napi_value(env, Buffer::from(b)) },
    }
  }
}

type MessageTsfn = ThreadsafeFunction<WsMessageArg, (), WsMessageArg, napi::Status, false, true, 0>;
type CloseTsfn = ThreadsafeFunction<WsCloseArg, (), WsCloseArg, napi::Status, false, true, 0>;

/// Cross-thread arg for close handlers: `(code?, reason?)`.
pub struct WsCloseArg {
  code: Option<u32>,
  reason: Option<String>,
}

#[napi(object)]
pub struct WsCloseInfo {
  pub code: Option<u32>,
  pub reason: Option<String>,
}

impl ToNapiValue for WsCloseArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    unsafe {
      WsCloseInfo::to_napi_value(
        env,
        WsCloseInfo {
          code: val.code,
          reason: val.reason,
        },
      )
    }
  }
}

fn to_ws_message(message: Either<String, Buffer>) -> WsMessage {
  match message {
    Either::A(s) => WsMessage::Text(s),
    Either::B(b) => WsMessage::Binary(b.to_vec()),
  }
}

/// Playwright `WebSocketRoute` — the page-side route handle.
#[napi]
pub struct WebSocketRoute {
  inner: CoreRoute,
}

impl WebSocketRoute {
  pub(crate) fn wrap(inner: CoreRoute) -> Self {
    Self { inner }
  }
}

#[napi]
impl WebSocketRoute {
  /// Playwright: `webSocketRoute.url()`.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Playwright: `webSocketRoute.send(message)`. Sends to the page.
  #[napi(ts_args_type = "message: string | Buffer")]
  pub async fn send(&self, message: Either<String, Buffer>) {
    self.inner.send(to_ws_message(message)).await;
  }

  /// Playwright: `webSocketRoute.close({ code?, reason? })`.
  #[napi]
  pub async fn close(&self, options: Option<WebSocketCloseOptions>) {
    let (code, reason) = options.map_or((None, None), |o| (o.code, o.reason));
    self.inner.close(code, reason).await;
  }

  /// Playwright: `webSocketRoute.onMessage(handler)`.
  #[napi(ts_args_type = "handler: (message: string | Buffer) => void")]
  pub fn on_message(&self, handler: MessageTsfn) {
    let tsfn = Arc::new(handler);
    self.inner.on_message(Arc::new(move |msg| {
      tsfn.call(WsMessageArg(msg), ThreadsafeFunctionCallMode::NonBlocking);
    }));
  }

  /// Playwright: `webSocketRoute.onClose(handler)`.
  #[napi(ts_args_type = "handler: (info: { code?: number, reason?: string }) => void")]
  pub fn on_close(&self, handler: CloseTsfn) {
    let tsfn = Arc::new(handler);
    self.inner.on_close(Arc::new(move |code, reason| {
      tsfn.call(WsCloseArg { code, reason }, ThreadsafeFunctionCallMode::NonBlocking);
    }));
  }

  /// Playwright: `webSocketRoute.connectToServer()`.
  #[napi]
  pub fn connect_to_server(&self) -> WebSocketRouteServer {
    WebSocketRouteServer {
      inner: self.inner.connect_to_server(),
    }
  }
}

/// Server-side handle from `webSocketRoute.connectToServer()`.
#[napi]
pub struct WebSocketRouteServer {
  inner: CoreServer,
}

#[napi]
impl WebSocketRouteServer {
  /// Server-side `url()`.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Send a message to the upstream server.
  #[napi(ts_args_type = "message: string | Buffer")]
  pub async fn send(&self, message: Either<String, Buffer>) {
    self.inner.send(to_ws_message(message)).await;
  }

  /// Close the upstream connection.
  #[napi]
  pub async fn close(&self, options: Option<WebSocketCloseOptions>) {
    let (code, reason) = options.map_or((None, None), |o| (o.code, o.reason));
    self.inner.close(code, reason).await;
  }

  /// Server-side `onMessage(handler)`.
  #[napi(ts_args_type = "handler: (message: string | Buffer) => void")]
  pub fn on_message(&self, handler: MessageTsfn) {
    let tsfn = Arc::new(handler);
    self.inner.on_message(Arc::new(move |msg| {
      tsfn.call(WsMessageArg(msg), ThreadsafeFunctionCallMode::NonBlocking);
    }));
  }

  /// Server-side `onClose(handler)`.
  #[napi(ts_args_type = "handler: (info: { code?: number, reason?: string }) => void")]
  pub fn on_close(&self, handler: CloseTsfn) {
    let tsfn = Arc::new(handler);
    self.inner.on_close(Arc::new(move |code, reason| {
      tsfn.call(WsCloseArg { code, reason }, ThreadsafeFunctionCallMode::NonBlocking);
    }));
  }
}

/// Cross-thread arg delivering a [`WebSocketRoute`] to the JS
/// `routeWebSocket` handler.
pub struct WebSocketRouteArg(pub CoreRoute);

impl ToNapiValue for WebSocketRouteArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> napi::Result<napi::sys::napi_value> {
    unsafe { WebSocketRoute::to_napi_value(env, WebSocketRoute::wrap(val.0)) }
  }
}

/// Build a core WS route handler from a JS handler TSFN. The returned
/// closure resolves once the JS handler has run, so the driver can read
/// `connectToServer()` state before opening the socket. `call_async`
/// awaits the JS callback's synchronous body (where `onMessage` /
/// `connectToServer` are wired up).
pub(crate) fn build_ws_handler(
  handler: Function<'_, WebSocketRouteArg, ()>,
) -> napi::Result<ferridriver::web_socket_route::WsHandler> {
  let tsfn: ThreadsafeFunction<WebSocketRouteArg, (), WebSocketRouteArg, napi::Status, false, false, 0> = handler
    .build_threadsafe_function()
    .callee_handled::<false>()
    .weak::<false>()
    .max_queue_size::<0>()
    .build()?;
  let tsfn = Arc::new(tsfn);
  Ok(Arc::new(move |route| {
    let tsfn = tsfn.clone();
    Box::pin(async move {
      let _ = tsfn.call_async(WebSocketRouteArg(route)).await;
    })
  }))
}
