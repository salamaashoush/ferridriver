//! NAPI bindings for `ferridriver::network::{Request, Response, WebSocket}`.
//!
//! Each class wraps the live core type and forwards every method directly
//! — Rule 1 (Rust core is the source of truth, NAPI is a thin
//! delegator). Sync core getters become NAPI getters so JS sees
//! `request.url` (no parens) only where Playwright also exposes a
//! getter; everywhere Playwright exposes a method we keep the method
//! shape so `request.url()` matches the canonical `test.d.ts`.

use ferridriver::network::{
  HeaderEntry as CoreHeaderEntry, RemoteAddr as CoreRemoteAddr, Request as CoreRequest, RequestSizes as CoreSizes,
  RequestTiming as CoreTiming, Response as CoreResponse, SecurityDetails as CoreSecurityDetails,
  WebSocket as CoreWebSocket, WebSocketEvent, WebSocketPayload,
};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use rustc_hash::FxHashMap;
use std::sync::Arc;

// ── HeaderEntry / Headers wire shapes ─────────────────────────────────

/// `{ name, value }` entries used by `headersArray()` (Playwright's
/// `HeadersArray` from `@isomorphic/types`).
#[napi(object)]
pub struct HeaderEntry {
  pub name: String,
  pub value: String,
}

impl From<CoreHeaderEntry> for HeaderEntry {
  fn from(h: CoreHeaderEntry) -> Self {
    Self {
      name: h.name,
      value: h.value,
    }
  }
}

/// Resource timing struct mirroring Playwright's `ResourceTiming`.
#[napi(object)]
pub struct RequestTiming {
  pub start_time: f64,
  pub domain_lookup_start: f64,
  pub domain_lookup_end: f64,
  pub connect_start: f64,
  pub secure_connection_start: f64,
  pub connect_end: f64,
  pub request_start: f64,
  pub response_start: f64,
  pub response_end: f64,
}

impl From<CoreTiming> for RequestTiming {
  fn from(t: CoreTiming) -> Self {
    Self {
      start_time: t.start_time,
      domain_lookup_start: t.domain_lookup_start,
      domain_lookup_end: t.domain_lookup_end,
      connect_start: t.connect_start,
      secure_connection_start: t.secure_connection_start,
      connect_end: t.connect_end,
      request_start: t.request_start,
      response_start: t.response_start,
      response_end: t.response_end,
    }
  }
}

#[napi(object)]
pub struct RequestSizes {
  #[napi(js_name = "requestBodySize")]
  pub request_body: u32,
  #[napi(js_name = "requestHeadersSize")]
  pub request_headers: u32,
  #[napi(js_name = "responseBodySize")]
  pub response_body: u32,
  #[napi(js_name = "responseHeadersSize")]
  pub response_headers: u32,
}

impl From<CoreSizes> for RequestSizes {
  fn from(s: CoreSizes) -> Self {
    Self {
      request_body: u32::try_from(s.request_body).unwrap_or(u32::MAX),
      request_headers: u32::try_from(s.request_headers).unwrap_or(u32::MAX),
      response_body: u32::try_from(s.response_body).unwrap_or(u32::MAX),
      response_headers: u32::try_from(s.response_headers).unwrap_or(u32::MAX),
    }
  }
}

#[napi(object)]
pub struct RemoteAddr {
  pub ip_address: String,
  pub port: u32,
}

impl From<CoreRemoteAddr> for RemoteAddr {
  fn from(a: CoreRemoteAddr) -> Self {
    Self {
      ip_address: a.ip_address,
      port: u32::from(a.port),
    }
  }
}

#[napi(object)]
pub struct SecurityDetails {
  pub protocol: Option<String>,
  pub subject_name: Option<String>,
  pub issuer: Option<String>,
  pub valid_from: Option<f64>,
  pub valid_to: Option<f64>,
}

impl From<CoreSecurityDetails> for SecurityDetails {
  fn from(s: CoreSecurityDetails) -> Self {
    Self {
      protocol: s.protocol,
      subject_name: s.subject_name,
      issuer: s.issuer,
      valid_from: s.valid_from,
      valid_to: s.valid_to,
    }
  }
}

#[napi(object)]
pub struct RequestFailure {
  pub error_text: String,
}

// ── Request ────────────────────────────────────────────────────────────

#[napi]
pub struct Request {
  pub(crate) inner: CoreRequest,
  /// Page that owns the request, used for `frame()` resolution. `None`
  /// when the wrapper was constructed without page context (e.g. raw
  /// `Request::new` in tests).
  pub(crate) page: Option<Arc<ferridriver::Page>>,
}

impl Request {
  pub(crate) fn from_core(inner: CoreRequest) -> Self {
    Self { inner, page: None }
  }

  pub(crate) fn from_core_with_page(inner: CoreRequest, page: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      page: Some(page),
    }
  }
}

#[napi]
impl Request {
  /// Mirrors Playwright `request.url(): string`.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Mirrors Playwright `request.method(): string`.
  #[napi]
  pub fn method(&self) -> String {
    self.inner.method().to_string()
  }

  /// Mirrors Playwright `request.resourceType(): string`.
  #[napi]
  pub fn resource_type(&self) -> String {
    self.inner.resource_type().to_string()
  }

  /// Mirrors Playwright `request.isNavigationRequest(): boolean`.
  #[napi]
  pub fn is_navigation_request(&self) -> bool {
    self.inner.is_navigation_request()
  }

  /// Mirrors Playwright `request.postData(): string | null`.
  #[napi(ts_return_type = "string | null")]
  pub fn post_data(&self) -> Option<String> {
    self.inner.post_data()
  }

  /// Mirrors Playwright `request.postDataBuffer(): Buffer | null`.
  #[napi(ts_return_type = "Buffer | null")]
  pub fn post_data_buffer(&self) -> Option<Buffer> {
    self.inner.post_data_buffer().map(Buffer::from)
  }

  /// Mirrors Playwright `request.postDataJSON(): any`.
  #[napi(js_name = "postDataJSON", ts_return_type = "any")]
  pub fn post_data_json(&self) -> Result<serde_json::Value> {
    self
      .inner
      .post_data_json()
      .map(|v| v.unwrap_or(serde_json::Value::Null))
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `request.headers(): Record<string, string>`.
  #[napi(ts_return_type = "Record<string, string>")]
  pub fn headers(&self) -> FxHashMap<String, String> {
    self.inner.headers()
  }

  /// Mirrors Playwright `request.headersArray(): Promise<HeadersArray>`.
  #[napi(ts_return_type = "Promise<{ name: string; value: string }[]>")]
  pub async fn headers_array(&self) -> Vec<HeaderEntry> {
    self.inner.headers_array().await.into_iter().map(Into::into).collect()
  }

  /// Mirrors Playwright `request.allHeaders(): Promise<Headers>`.
  #[napi(ts_return_type = "Promise<Record<string, string>>")]
  pub async fn all_headers(&self) -> Result<FxHashMap<String, String>> {
    self
      .inner
      .all_headers()
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `request.headerValue(name): Promise<string | null>`.
  #[napi(ts_return_type = "Promise<string | null>")]
  pub async fn header_value(&self, name: String) -> Result<Option<String>> {
    self
      .inner
      .header_value(&name)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `request.failure(): { errorText: string } | null` (sync).
  #[napi]
  pub fn failure(&self) -> Option<RequestFailure> {
    self.inner.failure().map(|t| RequestFailure { error_text: t })
  }

  /// Mirrors Playwright `request.timing(): ResourceTiming` (sync).
  #[napi]
  pub fn timing(&self) -> RequestTiming {
    self.inner.timing().into()
  }

  /// Mirrors Playwright `request.sizes(): Promise<RequestSizes>`.
  #[napi]
  pub async fn sizes(&self) -> Result<RequestSizes> {
    self
      .inner
      .sizes()
      .await
      .map(Into::into)
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `request.redirectedFrom(): Request | null`.
  /// Propagates the page reference so `.frame()` keeps working through
  /// the redirect chain.
  #[napi(ts_return_type = "Request | null")]
  pub fn redirected_from(&self) -> Option<Request> {
    self.inner.redirected_from().map(|r| match self.page.as_ref() {
      Some(page) => Request::from_core_with_page(r, page.clone()),
      None => Request::from_core(r),
    })
  }

  /// Mirrors Playwright `request.redirectedTo(): Request | null` (sync).
  #[napi(ts_return_type = "Request | null")]
  pub fn redirected_to(&self) -> Option<Request> {
    self.inner.redirected_to().map(|r| match self.page.as_ref() {
      Some(page) => Request::from_core_with_page(r, page.clone()),
      None => Request::from_core(r),
    })
  }

  /// Mirrors Playwright `request.response(): Promise<Response | null>`.
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn response(&self) -> Result<Option<Response>> {
    self
      .inner
      .response()
      .await
      .map(|opt| {
        opt.map(|r| match self.page.as_ref() {
          Some(page) => Response::from_core_with_page(r, page.clone()),
          None => Response::from_core(r),
        })
      })
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `request.existingResponse(): Response | null` —
  /// the already-received response, without waiting for one.
  #[napi(ts_return_type = "Promise<Response | null>")]
  pub async fn existing_response(&self) -> Option<Response> {
    self.inner.existing_response().await.map(|r| match self.page.as_ref() {
      Some(page) => Response::from_core_with_page(r, page.clone()),
      None => Response::from_core(r),
    })
  }

  /// Mirrors Playwright `request.frame(): Frame`. Resolves the
  /// initiating frame_id through the owning page's frame cache. Returns
  /// `null` when the request was emitted without a frame context (e.g.
  /// CDP main-target preload) or when the wrapper was created without a
  /// page reference.
  #[napi(ts_return_type = "Frame | null")]
  pub fn frame(&self) -> Option<crate::identity::FrameObject> {
    let page = self.page.as_ref()?;
    let frame_id = self.inner.frame_id()?;
    let frames = page.frames();
    for f in frames {
      if f.frame_id() == frame_id {
        return Some(crate::identity::FrameObject(f));
      }
    }
    None
  }

  /// Mirrors Playwright `request.serviceWorker(): Worker | null`.
  /// Service-worker request observability is a Tier-2 subsystem
  /// (`§2.7`); for now we always return `null` (no SW requests are
  /// surfaced as the originating worker yet).
  #[napi(ts_return_type = "null")]
  pub fn service_worker(&self) -> Null {
    Null
  }
}

// ── Response ───────────────────────────────────────────────────────────

#[napi]
pub struct Response {
  pub(crate) inner: CoreResponse,
  pub(crate) page: Option<Arc<ferridriver::Page>>,
}

impl Response {
  pub(crate) fn from_core(inner: CoreResponse) -> Self {
    Self { inner, page: None }
  }

  pub(crate) fn from_core_with_page(inner: CoreResponse, page: Arc<ferridriver::Page>) -> Self {
    Self {
      inner,
      page: Some(page),
    }
  }
}

#[napi]
impl Response {
  /// Mirrors Playwright `response.url(): string`.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Mirrors Playwright `response.status(): number`.
  #[napi]
  pub fn status(&self) -> i32 {
    i32::try_from(self.inner.status()).unwrap_or(i32::MAX)
  }

  /// Mirrors Playwright `response.statusText(): string`.
  #[napi]
  pub fn status_text(&self) -> String {
    self.inner.status_text().to_string()
  }

  /// Mirrors Playwright `response.ok(): boolean`.
  #[napi]
  pub fn ok(&self) -> bool {
    self.inner.ok()
  }

  /// Mirrors Playwright `response.fromServiceWorker(): boolean`.
  #[napi(js_name = "fromServiceWorker")]
  pub fn is_from_service_worker(&self) -> bool {
    self.inner.is_from_service_worker()
  }

  /// Mirrors Playwright `response.headers(): Record<string, string>`.
  #[napi(ts_return_type = "Record<string, string>")]
  pub fn headers(&self) -> FxHashMap<String, String> {
    self.inner.headers()
  }

  /// Mirrors Playwright `response.headersArray(): Promise<HeadersArray>`.
  #[napi(ts_return_type = "Promise<{ name: string; value: string }[]>")]
  pub async fn headers_array(&self) -> Vec<HeaderEntry> {
    self.inner.headers_array().await.into_iter().map(Into::into).collect()
  }

  /// Mirrors Playwright `response.allHeaders(): Promise<Headers>`.
  #[napi(ts_return_type = "Promise<Record<string, string>>")]
  pub async fn all_headers(&self) -> Result<FxHashMap<String, String>> {
    self
      .inner
      .all_headers()
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `response.headerValue(name): Promise<string | null>`.
  #[napi(ts_return_type = "Promise<string | null>")]
  pub async fn header_value(&self, name: String) -> Result<Option<String>> {
    self
      .inner
      .header_value(&name)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `response.headerValues(name): Promise<string[]>`.
  #[napi]
  pub async fn header_values(&self, name: String) -> Result<Vec<String>> {
    self
      .inner
      .header_values(&name)
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `response.body(): Promise<Buffer>`.
  #[napi]
  pub async fn body(&self) -> Result<Buffer> {
    self
      .inner
      .body()
      .await
      .map(Buffer::from)
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `response.text(): Promise<string>`.
  #[napi]
  pub async fn text(&self) -> Result<String> {
    self
      .inner
      .text()
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `response.json(): Promise<any>`.
  #[napi(ts_return_type = "Promise<any>")]
  pub async fn json(&self) -> Result<serde_json::Value> {
    self
      .inner
      .json()
      .await
      .map_err(|e| napi::Error::from_reason(e.to_string()))
  }

  /// Mirrors Playwright `response.finished(): Promise<null | Error>`.
  /// Resolves to `null` on success and to a JS `Error` on failure —
  /// matches Playwright exactly.
  #[napi(ts_return_type = "Promise<null | Error>")]
  pub async fn finished(&self) -> Result<Null> {
    match self.inner.finished().await {
      Ok(()) => Ok(Null),
      Err(e) => Err(crate::error::to_napi(e)),
    }
  }

  /// Mirrors Playwright `response.serverAddr(): Promise<RemoteAddr | null>`.
  #[napi(ts_return_type = "Promise<{ ipAddress: string; port: number } | null>")]
  pub async fn server_addr(&self) -> Option<RemoteAddr> {
    self.inner.server_addr().await.map(Into::into)
  }

  /// Mirrors Playwright `response.securityDetails(): Promise<SecurityDetails | null>`.
  #[napi(
    ts_return_type = "Promise<{ protocol?: string; subjectName?: string; issuer?: string; validFrom?: number; validTo?: number } | null>"
  )]
  pub async fn security_details(&self) -> Option<SecurityDetails> {
    self.inner.security_details().await.map(Into::into)
  }

  /// Mirrors Playwright `response.httpVersion(): Promise<string>`.
  /// Empty string when the backend did not report a protocol version.
  #[napi(ts_return_type = "Promise<string>")]
  pub async fn http_version(&self) -> String {
    self.inner.http_version().await.unwrap_or_default()
  }

  /// Mirrors Playwright `response.request(): Request`.
  #[napi]
  pub fn request(&self) -> Request {
    match self.page.as_ref() {
      Some(page) => Request::from_core_with_page(self.inner.request(), page.clone()),
      None => Request::from_core(self.inner.request()),
    }
  }

  /// Mirrors Playwright `response.frame(): Frame`. Convenience for
  /// `response.request().frame()`.
  #[napi(ts_return_type = "Frame | null")]
  pub fn frame(&self) -> Option<crate::identity::FrameObject> {
    self.request().frame()
  }
}

// ── WebSocket ──────────────────────────────────────────────────────────

#[napi]
pub struct WebSocket {
  pub(crate) inner: CoreWebSocket,
  /// `on` / `once` registrations, so `off(event, listener)` finds the
  /// core listener from JS function identity.
  listener_regs: Arc<std::sync::Mutex<Vec<WebSocketListenerReg>>>,
}

struct WebSocketListenerReg {
  event: String,
  id: u64,
  fn_ref: FunctionRef<WebSocketEventArg, ()>,
}

impl WebSocket {
  pub(crate) fn from_core(inner: CoreWebSocket) -> Self {
    Self {
      inner,
      listener_regs: Arc::new(std::sync::Mutex::new(Vec::new())),
    }
  }
}

/// Playwright's `{ payload: string | Buffer }` for `'framesent'` and
/// `'framereceived'`.
#[napi(object)]
pub struct WebSocketFrame {
  #[napi(ts_type = "string | Buffer")]
  pub payload: Either<String, Buffer>,
}

/// What a WebSocket listener receives: the frame, the error text, or
/// the socket itself on `'close'` (Playwright's `WebSocketEventMap`).
pub enum WebSocketEventArg {
  Frame(WebSocketPayload),
  Error(String),
  Close(CoreWebSocket),
}

impl WebSocketEventArg {
  fn from_event(event: WebSocketEvent, socket: &CoreWebSocket) -> Self {
    match event {
      WebSocketEvent::FrameSent(payload) | WebSocketEvent::FrameReceived(payload) => Self::Frame(payload),
      WebSocketEvent::Error(message) => Self::Error(message),
      WebSocketEvent::Close => Self::Close(socket.clone()),
    }
  }
}

impl ToNapiValue for WebSocketEventArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, val: Self) -> Result<napi::sys::napi_value> {
    match val {
      WebSocketEventArg::Frame(payload) => {
        let payload = match payload {
          WebSocketPayload::Text(text) => Either::A(text),
          WebSocketPayload::Binary(bytes) => Either::B(Buffer::from(bytes)),
        };
        unsafe { WebSocketFrame::to_napi_value(env, WebSocketFrame { payload }) }
      },
      WebSocketEventArg::Error(message) => unsafe { String::to_napi_value(env, message) },
      WebSocketEventArg::Close(socket) => unsafe { WebSocket::to_napi_value(env, WebSocket::from_core(socket)) },
    }
  }
}

#[napi]
impl WebSocket {
  /// Mirrors Playwright `webSocket.url(): string`.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Mirrors Playwright `webSocket.isClosed(): boolean`.
  #[napi]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  /// Playwright: `webSocket.on(event, listener)`; returns the socket so
  /// registrations chain.
  #[napi(
    ts_args_type = "event: 'framesent' | 'framereceived' | 'socketerror' | 'close', listener: (data: { payload: string | Buffer } | string | WebSocket) => void",
    ts_return_type = "this"
  )]
  pub fn on<'env>(
    &self,
    this: This<'env>,
    event: String,
    listener: Function<'_, WebSocketEventArg, ()>,
  ) -> Result<Object<'env>> {
    self.register_listener(&event, listener, false)?;
    Ok(this.object)
  }

  /// Node's `addListener`, an alias of [`Self::on`].
  #[napi(
    ts_args_type = "event: 'framesent' | 'framereceived' | 'socketerror' | 'close', listener: (data: { payload: string | Buffer } | string | WebSocket) => void",
    ts_return_type = "this"
  )]
  pub fn add_listener<'env>(
    &self,
    this: This<'env>,
    event: String,
    listener: Function<'_, WebSocketEventArg, ()>,
  ) -> Result<Object<'env>> {
    self.on(this, event, listener)
  }

  /// One-shot [`Self::on`].
  #[napi(
    ts_args_type = "event: 'framesent' | 'framereceived' | 'socketerror' | 'close', listener: (data: { payload: string | Buffer } | string | WebSocket) => void",
    ts_return_type = "this"
  )]
  pub fn once<'env>(
    &self,
    this: This<'env>,
    event: String,
    listener: Function<'_, WebSocketEventArg, ()>,
  ) -> Result<Object<'env>> {
    self.register_listener(&event, listener, true)?;
    Ok(this.object)
  }

  /// Remove `listener` from `event`, or every listener of `event`.
  #[napi(
    ts_args_type = "event: 'framesent' | 'framereceived' | 'socketerror' | 'close', listener?: (data: { payload: string | Buffer } | string | WebSocket) => void",
    ts_return_type = "this"
  )]
  pub fn off<'env>(
    &self,
    env: Env,
    this: This<'env>,
    event: String,
    listener: Option<Function<'_, WebSocketEventArg, ()>>,
  ) -> Result<Object<'env>> {
    let mut regs = self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(listener) = listener else {
      self.inner.events().remove_listeners_named(&event);
      regs.retain(|r| r.event != event);
      return Ok(this.object);
    };
    let wanted = listener.create_ref()?;
    let mut i = 0;
    while i < regs.len() {
      let hit =
        regs[i].event == event && env.strict_equals(wanted.borrow_back(&env)?, regs[i].fn_ref.borrow_back(&env)?)?;
      if hit {
        let reg = regs.remove(i);
        self.inner.events().off(ferridriver::events::ListenerId(reg.id));
      } else {
        i += 1;
      }
    }
    Ok(this.object)
  }

  /// Node's `removeListener`, an alias of [`Self::off`].
  #[napi(
    ts_args_type = "event: 'framesent' | 'framereceived' | 'socketerror' | 'close', listener?: (data: { payload: string | Buffer } | string | WebSocket) => void",
    ts_return_type = "this"
  )]
  pub fn remove_listener<'env>(
    &self,
    env: Env,
    this: This<'env>,
    event: String,
    listener: Option<Function<'_, WebSocketEventArg, ()>>,
  ) -> Result<Object<'env>> {
    self.off(env, this, event, listener)
  }

  /// Playwright `webSocket.waitForEvent(event, optionsOrPredicate?)`:
  /// the next `event` the predicate accepts, in the shape a listener
  /// gets. Waiting for anything but `'close'` fails once the socket closes.
  #[napi(
    ts_args_type = "event: 'framesent' | 'framereceived' | 'socketerror' | 'close', optionsOrPredicate?: number | ((data: { payload: string | Buffer } | string | WebSocket) => boolean | Promise<boolean>) | { predicate?: (data: { payload: string | Buffer } | string | WebSocket) => boolean | Promise<boolean>; timeout?: number; signal?: AbortSignal }",
    ts_return_type = "Promise<{ payload: string | Buffer } | string | WebSocket>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn wait_for_event(
    &self,
    env: &Env,
    event: String,
    options_or_predicate: Option<Either3<f64, Function<'_, WebSocketEventArg, crate::page::PredReturn>, Object<'_>>>,
  ) -> Result<AsyncBlock<WebSocketEventArg>> {
    let mut timeout_ms: Option<f64> = None;
    let mut predicate = None;
    let mut signal = None;
    match options_or_predicate {
      None => {},
      Some(Either3::A(timeout)) => timeout_ms = Some(timeout),
      Some(Either3::B(f)) => {
        predicate = Some(
          f.build_threadsafe_function::<WebSocketEventArg>()
            .callee_handled::<false>()
            .weak::<false>()
            .max_queue_size::<0>()
            .build()?,
        );
      },
      Some(Either3::C(bag)) => {
        timeout_ms = bag.get::<f64>("timeout")?;
        signal = crate::abort::take(&mut Some(bag.get::<crate::abort::Signal>("signal")?));
        if let Some(f) = bag.get::<Function<'_, WebSocketEventArg, crate::page::PredReturn>>("predicate")? {
          predicate = Some(
            f.build_threadsafe_function::<WebSocketEventArg>()
              .callee_handled::<false>()
              .weak::<false>()
              .max_queue_size::<0>()
              .build()?,
          );
        }
      },
    }
    let timeout = crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0));
    let socket = self.inner.clone();
    let mut events = self.inner.subscribe();
    AsyncBlockBuilder::new(crate::abort::race(signal, async move {
      use ferridriver::events::EmitterEvent;
      if event != "close" && socket.is_closed() {
        return Err(napi::Error::from_reason("Socket closed"));
      }
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout);
      loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = if timeout == 0 {
          Ok(events.recv().await)
        } else {
          tokio::time::timeout(remaining, events.recv()).await
        };
        let Ok(Some(ev)) = next else {
          return Err(napi::Error::from_reason(format!(
            "Timeout {timeout}ms exceeded while waiting for event \"{event}\""
          )));
        };
        if !WebSocketEvent::matches_name(&event, &ev) {
          if matches!(ev, WebSocketEvent::Close) {
            return Err(napi::Error::from_reason("Socket closed"));
          }
          continue;
        }
        let Some(predicate) = &predicate else {
          return Ok(WebSocketEventArg::from_event(ev, &socket));
        };
        let accepted = predicate
          .call_async(WebSocketEventArg::from_event(ev.clone(), &socket))
          .await
          .map_err(|e| napi::Error::from_reason(e.to_string()))?;
        if crate::page::resolve_pred(accepted).await {
          return Ok(WebSocketEventArg::from_event(ev, &socket));
        }
      }
    }))
    .build(env)
  }
}

impl WebSocket {
  fn register_listener(&self, event: &str, listener: Function<'_, WebSocketEventArg, ()>, once: bool) -> Result<()> {
    let fn_ref = listener.create_ref()?;
    let tsfn = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    let socket = self.inner.clone();
    let callback: Arc<dyn Fn(WebSocketEvent) + Send + Sync> = Arc::new(move |ev| {
      tsfn.call(
        WebSocketEventArg::from_event(ev, &socket),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    });
    let id = if once {
      self.inner.events().once(event, callback)
    } else {
      self.inner.events().on(event, callback)
    };
    self
      .listener_regs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(WebSocketListenerReg {
        event: event.to_string(),
        id: id.0,
        fn_ref,
      });
    Ok(())
  }
}
