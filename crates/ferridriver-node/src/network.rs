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

  /// Mirrors Playwright `request.failure(): { errorText: string } | null`.
  #[napi(ts_return_type = "Promise<{ errorText: string } | null>")]
  pub async fn failure(&self) -> Option<RequestFailure> {
    self.inner.failure().await.map(|t| RequestFailure { error_text: t })
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

  /// Mirrors Playwright `request.frame(): Frame`. Resolves the
  /// initiating frame_id through the owning page's frame cache. Returns
  /// `null` when the request was emitted without a frame context (e.g.
  /// CDP main-target preload) or when the wrapper was created without a
  /// page reference.
  #[napi(ts_return_type = "Frame | null")]
  pub fn frame(&self) -> Option<crate::frame::Frame> {
    let page = self.page.as_ref()?;
    let frame_id = self.inner.frame_id()?;
    let frames = page.frames();
    for f in frames {
      if f.frame_id() == frame_id {
        return Some(crate::frame::Frame::wrap(f));
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
      Err(message) => Err(napi::Error::from_reason(message)),
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
  pub fn frame(&self) -> Option<crate::frame::Frame> {
    self.request().frame()
  }
}

// ── WebSocket ──────────────────────────────────────────────────────────

#[napi]
pub struct WebSocket {
  pub(crate) inner: CoreWebSocket,
}

impl WebSocket {
  pub(crate) fn from_core(inner: CoreWebSocket) -> Self {
    Self { inner }
  }
}

/// Synthetic outcome shape returned by `WebSocket.waitForEvent` for a
/// generic event subscription — JS can pattern-match on `type`.
#[napi(object)]
pub struct WebSocketEventDescriptor {
  pub event: String,
  #[napi(ts_type = "string | Buffer | null")]
  pub payload: Option<serde_json::Value>,
  pub error: Option<String>,
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

  /// Mirrors Playwright `webSocket.waitForEvent(event, options?)`.
  /// Resolves with the next matching event payload; rejects on timeout.
  #[napi(ts_return_type = "Promise<{ event: string; payload: string | Buffer | null; error: string | null }>")]
  pub async fn wait_for_event(&self, event: String, timeout_ms: Option<f64>) -> Result<WebSocketEventDescriptor> {
    let timeout = std::time::Duration::from_millis(crate::types::f64_to_u64(timeout_ms.unwrap_or(30000.0)));
    let mut rx = self.inner.subscribe();
    let event_lc = event.to_ascii_lowercase();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err(napi::Error::from_reason(format!(
          "Timeout {}ms exceeded while waiting for WebSocket event {event:?}",
          timeout.as_millis()
        )));
      }
      match tokio::time::timeout(remaining, rx.recv()).await {
        Ok(Ok(ev)) => {
          if let Some(out) = match_event(&event_lc, &ev) {
            return Ok(out);
          }
        },
        Ok(Err(_)) => {
          return Err(napi::Error::from_reason("WebSocket channel closed"));
        },
        Err(_) => {
          return Err(napi::Error::from_reason(format!(
            "Timeout {}ms exceeded while waiting for WebSocket event {event:?}",
            timeout.as_millis()
          )));
        },
      }
    }
  }
}

fn match_event(name: &str, ev: &WebSocketEvent) -> Option<WebSocketEventDescriptor> {
  match (name, ev) {
    ("framesent", WebSocketEvent::FrameSent(p)) => Some(WebSocketEventDescriptor {
      event: "framesent".into(),
      payload: Some(payload_to_json(p)),
      error: None,
    }),
    ("framereceived", WebSocketEvent::FrameReceived(p)) => Some(WebSocketEventDescriptor {
      event: "framereceived".into(),
      payload: Some(payload_to_json(p)),
      error: None,
    }),
    ("socketerror", WebSocketEvent::Error(msg)) => Some(WebSocketEventDescriptor {
      event: "socketerror".into(),
      payload: None,
      error: Some(msg.clone()),
    }),
    ("close", WebSocketEvent::Close) => Some(WebSocketEventDescriptor {
      event: "close".into(),
      payload: None,
      error: None,
    }),
    _ => None,
  }
}

fn payload_to_json(p: &WebSocketPayload) -> serde_json::Value {
  match p {
    WebSocketPayload::Text(s) => serde_json::Value::String(s.clone()),
    WebSocketPayload::Binary(b) => {
      serde_json::Value::String(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b))
    },
  }
}
