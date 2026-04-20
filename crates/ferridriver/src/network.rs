//! Network lifecycle objects: `Request`, `Response`, `WebSocket`.
//!
//! Mirrors Playwright's client-side `Request` / `Response` / `WebSocket`
//! classes from `packages/playwright-core/src/client/network.ts`. Live
//! object references — listeners hold a `Request` and can call
//! `request.response().await` later; the future resolves when the
//! response (or failure) is recorded by the backend.
//!
//! Each lifecycle object wraps an `Arc<RequestState>` /
//! `Arc<ResponseState>` / `Arc<WebSocketState>`. The same `Arc` is held
//! by the per-page network listener loop on every backend; backend
//! events mutate the inner state through the lock and notify waiters.

use crate::error::{FerriError, Result};
use arc_swap::{ArcSwap, ArcSwapOption};
use rustc_hash::FxHashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex as AsyncMutex, Notify, RwLock};

/// Header map. Lower-case-keyed combined view (matches Playwright's
/// `headers(): Headers` shape).
pub type Headers = FxHashMap<String, String>;

/// Single header entry preserving original case and duplicate keys.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HeaderEntry {
  pub name: String,
  pub value: String,
}

/// Resource timing in milliseconds since `startTime`. Sentinel values:
/// `start_time` is wall-clock ms (Unix epoch), every other field is `-1.0`
/// when not measured (matches Playwright's `ResourceTiming`).
#[derive(Debug, Clone, Copy, serde::Serialize)]
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

impl RequestTiming {
  fn empty() -> Self {
    Self {
      start_time: 0.0,
      domain_lookup_start: -1.0,
      domain_lookup_end: -1.0,
      connect_start: -1.0,
      secure_connection_start: -1.0,
      connect_end: -1.0,
      request_start: -1.0,
      response_start: -1.0,
      response_end: -1.0,
    }
  }
}

impl Default for RequestTiming {
  fn default() -> Self {
    Self::empty()
  }
}

/// Body / header byte counts for a completed request. Field names mirror
/// Playwright's `RequestSizes` over the wire (camelCase via serde rename);
/// the Rust field identifiers drop the `_size` suffix so they read
/// cleanly at call sites without all four sharing a postfix.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct RequestSizes {
  #[serde(rename = "requestBodySize")]
  pub request_body: u64,
  #[serde(rename = "requestHeadersSize")]
  pub request_headers: u64,
  #[serde(rename = "responseBodySize")]
  pub response_body: u64,
  #[serde(rename = "responseHeadersSize")]
  pub response_headers: u64,
}

/// Server IP + port. Resolved from CDP `responseReceived.response.remoteIPAddress`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteAddr {
  pub ip_address: String,
  pub port: u16,
}

/// TLS certificate info. Resolved from CDP `responseReceived.response.securityDetails`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SecurityDetails {
  pub protocol: Option<String>,
  pub subject_name: Option<String>,
  pub issuer: Option<String>,
  pub valid_from: Option<f64>,
  pub valid_to: Option<f64>,
}

// ── BodyFetcher ─────────────────────────────────────────────────────────────

/// Backend-supplied async closure that fetches a response body lazily.
/// Used by `Response::body()` so the actual `Network.getResponseBody`
/// (CDP) / `network.fetchBodyBytes` (`BiDi`) round-trip happens on demand,
/// not as part of every `responseReceived` event.
pub type BodyFn = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send>> + Send + Sync>;

/// Backend-supplied async closure that fetches actual headers (after
/// HSTS, cookie injection, redirects). CDP delivers them via push event;
/// for backends without push, the `Response` calls this lazily.
pub type RawHeadersFn = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Vec<HeaderEntry>>> + Send>> + Send + Sync>;

/// Returns a `BodyFn` that reports the operation as unsupported. Used by
/// backends that genuinely can't expose response bodies (stock
/// `WKWebView` — no public API for `loadResource:` interception on
/// main-document navigations).
#[must_use]
pub fn body_unsupported(reason: &'static str) -> BodyFn {
  Arc::new(move || {
    let reason = reason.to_string();
    Box::pin(async move { Err(FerriError::Unsupported(reason)) })
  })
}

// ── Request ─────────────────────────────────────────────────────────────────

/// Live reference to a network request. Mirrors Playwright's `Request`.
///
/// Cheaply cloneable; every clone shares the same underlying state. Sync
/// getters (`url`, `method`, ...) read immutable fields. Async accessors
/// (`response`, `all_headers`) wait until the backend records the result.
#[derive(Clone)]
pub struct Request {
  inner: Arc<RequestState>,
}

impl std::fmt::Debug for Request {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Request")
      .field("id", &self.inner.id)
      .field("method", &self.inner.method)
      .field("url", &self.inner.url)
      .field("resource_type", &self.inner.resource_type)
      .finish()
  }
}

pub(crate) struct RequestState {
  // Immutable — set at construction.
  id: String,
  url: String,
  method: String,
  resource_type: String,
  is_navigation_request: bool,
  post_data: Option<Vec<u8>>,
  provisional_headers: Headers,
  frame_id: Option<String>,
  redirected_from: Option<Arc<RequestState>>,

  // Sync-readable mutable fields — `ArcSwap` so Playwright-style sync
  // accessors (`timing()`, `redirectedTo()`, `sizes()` after response)
  // stay sync.
  timing: ArcSwap<RequestTiming>,
  sizes: ArcSwap<RequestSizes>,
  redirected_to: ArcSwapOption<RequestState>,

  // Async-only mutable state (guarded by `state`).
  state: RwLock<RequestMutState>,

  // Wakers — `notify_waiters()` on every state transition.
  outcome_notify: Notify,
  headers_notify: Notify,

  // Async fetchers (closure-typed so backend types stay private).
  raw_headers_fn: AsyncMutex<Option<RawHeadersFn>>,
}

struct RequestMutState {
  raw_headers: Option<Vec<HeaderEntry>>,
  response: Option<Arc<ResponseState>>,
  failure: Option<String>,
}

impl Request {
  /// Construct a fresh `Request`. Called by backend listeners on the
  /// initial `requestWillBeSent` / `beforeRequestSent` event. When
  /// `init.redirected_from` is set, the constructor wires the prior
  /// request's `redirected_to` slot synchronously via `ArcSwapOption`.
  #[must_use]
  pub fn new(init: RequestInit) -> Self {
    let inner = Arc::new(RequestState {
      id: init.id,
      url: init.url,
      method: init.method,
      resource_type: init.resource_type,
      is_navigation_request: init.is_navigation_request,
      post_data: init.post_data,
      provisional_headers: init.headers,
      frame_id: init.frame_id,
      redirected_from: init.redirected_from.map(|r| r.inner),
      timing: ArcSwap::from_pointee(init.timing.unwrap_or_default()),
      sizes: ArcSwap::from_pointee(RequestSizes::default()),
      redirected_to: ArcSwapOption::const_empty(),
      state: RwLock::new(RequestMutState {
        raw_headers: None,
        response: None,
        failure: None,
      }),
      outcome_notify: Notify::new(),
      headers_notify: Notify::new(),
      raw_headers_fn: AsyncMutex::new(init.raw_headers_fn),
    });

    // Sync redirect chain link: when the new request is constructed
    // off a `redirected_from`, point that prior request's `redirected_to`
    // slot at the new one. `ArcSwapOption::store` is sync — no need for
    // the listener to make a separate `link_redirect_chain().await` call.
    if let Some(prev) = inner.redirected_from.as_ref() {
      prev.redirected_to.store(Some(inner.clone()));
    }

    Self { inner }
  }

  // -- Sync getters ---------------------------------------------------------

  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }
  #[must_use]
  pub fn method(&self) -> &str {
    &self.inner.method
  }
  #[must_use]
  pub fn resource_type(&self) -> &str {
    &self.inner.resource_type
  }
  #[must_use]
  pub fn id(&self) -> &str {
    &self.inner.id
  }
  #[must_use]
  pub fn is_navigation_request(&self) -> bool {
    self.inner.is_navigation_request
  }
  #[must_use]
  pub fn frame_id(&self) -> Option<&str> {
    self.inner.frame_id.as_deref()
  }

  /// POST body as UTF-8 text (matches Playwright's `postData()`). Returns
  /// `None` for non-POST requests or non-UTF-8 bodies.
  #[must_use]
  pub fn post_data(&self) -> Option<String> {
    self
      .inner
      .post_data
      .as_ref()
      .and_then(|b| std::str::from_utf8(b).ok().map(std::string::ToString::to_string))
  }

  /// POST body as raw bytes (matches Playwright's `postDataBuffer()`).
  #[must_use]
  pub fn post_data_buffer(&self) -> Option<Vec<u8>> {
    self.inner.post_data.clone()
  }

  /// POST body parsed as JSON or `application/x-www-form-urlencoded`.
  /// Mirrors Playwright's `postDataJSON()`: returns the parsed value or
  /// `None` if there's no body. Errors if the body is not valid JSON for
  /// non-form content types.
  ///
  /// # Errors
  ///
  /// Returns an error if the post data is set but cannot be parsed as JSON
  /// (and the content type is not `application/x-www-form-urlencoded`).
  pub fn post_data_json(&self) -> Result<Option<serde_json::Value>> {
    let Some(body) = self.post_data() else {
      return Ok(None);
    };
    if let Some(ct) = self.headers().get("content-type") {
      if ct.contains("application/x-www-form-urlencoded") {
        let mut entries = serde_json::Map::new();
        for (k, v) in url_decode_form(&body) {
          entries.insert(k, serde_json::Value::String(v));
        }
        return Ok(Some(serde_json::Value::Object(entries)));
      }
    }
    serde_json::from_str(&body)
      .map(Some)
      .map_err(|_| FerriError::Other(format!("POST data is not a valid JSON object: {body}")))
  }

  /// Provisional headers (matches Playwright's deprecated `headers()`).
  /// For the post-extraInfo headers use `all_headers()`.
  #[must_use]
  pub fn headers(&self) -> Headers {
    self.inner.provisional_headers.clone()
  }

  /// Headers as `[{name, value}]` preserving order and duplicates.
  /// Returns provisional headers when raw headers haven't arrived yet.
  pub async fn headers_array(&self) -> Vec<HeaderEntry> {
    let state = self.inner.state.read().await;
    if let Some(raw) = state.raw_headers.clone() {
      return raw;
    }
    drop(state);
    headers_to_array(&self.inner.provisional_headers)
  }

  /// All headers, awaiting raw header push or fetch if needed.
  ///
  /// # Errors
  ///
  /// Returns an error if the raw-header fetcher fails.
  pub async fn all_headers(&self) -> Result<Headers> {
    let raw = self.fetch_raw_headers().await?;
    Ok(headers_array_to_map(&raw))
  }

  /// Single header value (case-insensitive). Returns `None` when the
  /// header is absent. Multiple values are joined with `, ` (or `\n` for
  /// `Set-Cookie`) per Playwright's `headerValue()` contract.
  ///
  /// # Errors
  ///
  /// Returns an error if the raw-header fetcher fails.
  pub async fn header_value(&self, name: &str) -> Result<Option<String>> {
    let raw = self.fetch_raw_headers().await?;
    Ok(get_header_value(&raw, name))
  }

  async fn fetch_raw_headers(&self) -> Result<Vec<HeaderEntry>> {
    {
      let state = self.inner.state.read().await;
      if let Some(raw) = state.raw_headers.clone() {
        return Ok(raw);
      }
    }
    if let Some(f) = self.inner.raw_headers_fn.lock().await.clone() {
      let raw = f().await?;
      let mut state = self.inner.state.write().await;
      if state.raw_headers.is_none() {
        state.raw_headers = Some(raw.clone());
      }
      self.inner.headers_notify.notify_waiters();
      return Ok(raw);
    }
    Ok(headers_to_array(&self.inner.provisional_headers))
  }

  // -- Service worker -----------------------------------------------------

  /// Mirrors Playwright `request.serviceWorker(): Worker | null` —
  /// returns the Service Worker that initiated the request (if any).
  /// Service-worker request observability is a Tier-2 subsystem
  /// (`§2.7`), so we always return `None` for now. The signature
  /// matches Playwright so flipping it on later is non-breaking; the
  /// body genuinely doesn't need `self` until `§2.7` wires per-request
  /// `SW` provenance, which is why the lint suppression below is local
  /// and justified rather than a workspace-wide escape hatch.
  #[must_use]
  #[allow(clippy::unused_self)] // Playwright-required method shape; body fills in at §2.7.
  pub const fn service_worker(&self) -> Option<()> {
    None
  }

  // -- Redirect chain ------------------------------------------------------

  #[must_use]
  pub fn redirected_from(&self) -> Option<Request> {
    self
      .inner
      .redirected_from
      .as_ref()
      .map(|r| Request { inner: r.clone() })
  }

  /// Sync — Playwright's `Request.redirectedTo(): Request | null`.
  /// Reads via `ArcSwapOption` so no async lock is needed even though
  /// the slot is set after construction by the listener.
  #[must_use]
  pub fn redirected_to(&self) -> Option<Request> {
    self.inner.redirected_to.load_full().map(|r| Request { inner: r })
  }

  // -- Outcome -------------------------------------------------------------

  /// Failure text, if the request failed before producing a response.
  pub async fn failure(&self) -> Option<String> {
    self.inner.state.read().await.failure.clone()
  }

  /// Wait for the response (or failure) to be recorded. Mirrors
  /// Playwright's `request.response(): Promise<Response | null>`.
  ///
  /// Returns `Ok(None)` for a request that failed.
  ///
  /// # Errors
  ///
  /// Currently infallible (preserved for forward-compat with cancellation).
  pub async fn response(&self) -> Result<Option<Response>> {
    loop {
      let waiter = self.inner.outcome_notify.notified();
      tokio::pin!(waiter);
      // Re-arm before reading state so we don't miss a signal that
      // races with the read.
      waiter.as_mut().enable();
      {
        let state = self.inner.state.read().await;
        if let Some(r) = state.response.clone() {
          return Ok(Some(Response { inner: r }));
        }
        if state.failure.is_some() {
          return Ok(None);
        }
      }
      waiter.await;
    }
  }

  /// Existing response without waiting (matches Playwright's
  /// `_response` private accessor used by the test runner).
  pub async fn existing_response(&self) -> Option<Response> {
    self
      .inner
      .state
      .read()
      .await
      .response
      .clone()
      .map(|r| Response { inner: r })
  }

  // -- Timing / sizes ------------------------------------------------------

  /// Sync — Playwright's `Request.timing(): ResourceTiming`. The
  /// underlying `ArcSwap` is updated by the backend listener as
  /// timing samples arrive.
  #[must_use]
  pub fn timing(&self) -> RequestTiming {
    **self.inner.timing.load()
  }

  /// Per Playwright: throws if the request didn't reach response (no
  /// transfer happened). We match that contract.
  ///
  /// # Errors
  ///
  /// Returns an error if the request hasn't received a response.
  pub async fn sizes(&self) -> Result<RequestSizes> {
    let state = self.inner.state.read().await;
    if state.response.is_none() {
      return Err(FerriError::Other("Unable to fetch sizes for failed request".into()));
    }
    Ok(**self.inner.sizes.load())
  }

  // -- Internal mutators (called by backend listeners) ---------------------

  /// Record raw headers from `requestWillBeSentExtraInfo` / equivalent.
  pub async fn set_raw_headers(&self, raw: Vec<HeaderEntry>) {
    let mut state = self.inner.state.write().await;
    state.raw_headers = Some(raw);
    drop(state);
    self.inner.headers_notify.notify_waiters();
  }

  pub fn update_timing(&self, timing: RequestTiming) {
    self.inner.timing.store(Arc::new(timing));
  }

  pub fn update_sizes(&self, sizes: RequestSizes) {
    self.inner.sizes.store(Arc::new(sizes));
  }

  pub async fn set_response(&self, response: &Response) {
    let mut state = self.inner.state.write().await;
    state.response = Some(response.inner.clone());
    drop(state);
    self.inner.outcome_notify.notify_waiters();
  }

  pub async fn set_failure(&self, error_text: String) {
    let mut state = self.inner.state.write().await;
    state.failure = Some(error_text);
    drop(state);
    self.inner.outcome_notify.notify_waiters();
  }

  /// JSON snapshot of the request's current state. Used by MCP's
  /// `diagnostics(type=network)` tool which serialises the per-context
  /// network log to a flat array of strings — the live `Request`
  /// object isn't itself `serde::Serialize` because its waiters / locks
  /// would have to be excluded anyway.
  pub async fn to_diagnostic_json(&self) -> serde_json::Value {
    let state = self.inner.state.read().await;
    serde_json::json!({
      "id": self.inner.id,
      "method": self.inner.method,
      "url": self.inner.url,
      "resourceType": self.inner.resource_type,
      "isNavigationRequest": self.inner.is_navigation_request,
      "status": state.response.as_ref().map(|r| r.status),
      "mimeType": state
        .response
        .as_ref()
        .and_then(|r| r.provisional_headers.get("content-type").cloned()),
      "headers": self.inner.provisional_headers,
      "postData": self.post_data(),
      "failure": state.failure,
    })
  }
}

/// Construction parameters for a new `Request`.
pub struct RequestInit {
  pub id: String,
  pub url: String,
  pub method: String,
  pub resource_type: String,
  pub is_navigation_request: bool,
  pub post_data: Option<Vec<u8>>,
  pub headers: Headers,
  pub frame_id: Option<String>,
  pub redirected_from: Option<Request>,
  pub timing: Option<RequestTiming>,
  pub raw_headers_fn: Option<RawHeadersFn>,
}

// ── Response ────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Response {
  inner: Arc<ResponseState>,
}

impl std::fmt::Debug for Response {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Response")
      .field("status", &self.inner.status)
      .field("status_text", &self.inner.status_text)
      .field("url", &self.inner.url)
      .finish()
  }
}

pub(crate) struct ResponseState {
  request: Arc<RequestState>,
  url: String,
  status: i64,
  status_text: String,
  from_service_worker: bool,
  http_version: Option<String>,
  provisional_headers: Headers,

  state: RwLock<ResponseMutState>,
  finished_notify: Notify,

  body_fn: AsyncMutex<Option<BodyFn>>,
  raw_headers_fn: AsyncMutex<Option<RawHeadersFn>>,
}

struct ResponseMutState {
  raw_headers: Option<Vec<HeaderEntry>>,
  body_cache: Option<Vec<u8>>,
  remote_addr: Option<RemoteAddr>,
  security_details: Option<SecurityDetails>,
  finished: Option<std::result::Result<(), String>>,
}

impl Response {
  #[must_use]
  pub fn new(init: ResponseInit) -> Self {
    let inner = Arc::new(ResponseState {
      request: init.request.inner.clone(),
      url: init.url,
      status: init.status,
      status_text: init.status_text,
      from_service_worker: init.from_service_worker,
      http_version: init.http_version,
      provisional_headers: init.headers,
      state: RwLock::new(ResponseMutState {
        raw_headers: None,
        body_cache: None,
        remote_addr: init.remote_addr,
        security_details: init.security_details,
        finished: None,
      }),
      finished_notify: Notify::new(),
      body_fn: AsyncMutex::new(init.body_fn),
      raw_headers_fn: AsyncMutex::new(init.raw_headers_fn),
    });
    Self { inner }
  }

  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }
  #[must_use]
  pub fn status(&self) -> i64 {
    self.inner.status
  }
  #[must_use]
  pub fn status_text(&self) -> &str {
    &self.inner.status_text
  }
  /// Matches Playwright `ok()`: `0 || (200..=299)`.
  #[must_use]
  pub fn ok(&self) -> bool {
    self.inner.status == 0 || (200..=299).contains(&self.inner.status)
  }
  /// Mirrors Playwright's `response.fromServiceWorker(): boolean`. NAPI
  /// re-exports this as the Playwright-canonical method name.
  #[must_use]
  pub fn is_from_service_worker(&self) -> bool {
    self.inner.from_service_worker
  }
  #[must_use]
  pub fn request(&self) -> Request {
    Request {
      inner: self.inner.request.clone(),
    }
  }
  #[must_use]
  pub fn frame_id(&self) -> Option<&str> {
    self.inner.request.frame_id.as_deref()
  }

  #[must_use]
  pub fn headers(&self) -> Headers {
    self.inner.provisional_headers.clone()
  }

  /// All headers (raw if available, provisional otherwise).
  ///
  /// # Errors
  ///
  /// Returns an error if the raw-header fetcher fails.
  pub async fn all_headers(&self) -> Result<Headers> {
    let raw = self.fetch_raw_headers().await?;
    Ok(headers_array_to_map(&raw))
  }

  pub async fn headers_array(&self) -> Vec<HeaderEntry> {
    let state = self.inner.state.read().await;
    if let Some(raw) = state.raw_headers.clone() {
      return raw;
    }
    drop(state);
    headers_to_array(&self.inner.provisional_headers)
  }

  /// Single header value (case-insensitive). Per Playwright, multi-value
  /// headers are joined with `, ` (or `\n` for `Set-Cookie`).
  ///
  /// # Errors
  ///
  /// Returns an error if the raw-header fetcher fails.
  pub async fn header_value(&self, name: &str) -> Result<Option<String>> {
    let raw = self.fetch_raw_headers().await?;
    Ok(get_header_value(&raw, name))
  }

  /// All values for a given header name (case-insensitive, preserves
  /// duplicates).
  ///
  /// # Errors
  ///
  /// Returns an error if the raw-header fetcher fails.
  pub async fn header_values(&self, name: &str) -> Result<Vec<String>> {
    let raw = self.fetch_raw_headers().await?;
    let lc = name.to_ascii_lowercase();
    Ok(
      raw
        .iter()
        .filter(|h| h.name.to_ascii_lowercase() == lc)
        .map(|h| h.value.clone())
        .collect(),
    )
  }

  async fn fetch_raw_headers(&self) -> Result<Vec<HeaderEntry>> {
    {
      let state = self.inner.state.read().await;
      if let Some(raw) = state.raw_headers.clone() {
        return Ok(raw);
      }
    }
    if let Some(f) = self.inner.raw_headers_fn.lock().await.clone() {
      let raw = f().await?;
      let mut state = self.inner.state.write().await;
      if state.raw_headers.is_none() {
        state.raw_headers = Some(raw.clone());
      }
      return Ok(raw);
    }
    Ok(headers_to_array(&self.inner.provisional_headers))
  }

  /// Wait for `loadingFinished` / `loadingFailed`. Resolves to `Ok(())`
  /// on success or `Err` carrying the failure text.
  ///
  /// Mirrors Playwright's `response.finished(): Promise<null | Error>`.
  /// We expose `Result` rather than `Option<Error>` so the typed error
  /// flows through `?` cleanly in Rust callers; the NAPI layer converts
  /// to `Promise<null | Error>` at the boundary.
  ///
  /// # Errors
  ///
  /// Returns the backend-reported failure text when the underlying
  /// load failed (e.g. `loadingFailed.errorText` on CDP).
  pub async fn finished(&self) -> std::result::Result<(), String> {
    loop {
      let waiter = self.inner.finished_notify.notified();
      tokio::pin!(waiter);
      waiter.as_mut().enable();
      {
        let state = self.inner.state.read().await;
        if let Some(outcome) = state.finished.clone() {
          return outcome;
        }
      }
      waiter.await;
    }
  }

  /// Response body bytes. Cached after first call.
  ///
  /// # Errors
  ///
  /// Returns an error if the body fetcher fails or the backend doesn't
  /// support body retrieval (typed `FerriError::Unsupported`).
  pub async fn body(&self) -> Result<Vec<u8>> {
    {
      let state = self.inner.state.read().await;
      if let Some(b) = state.body_cache.clone() {
        return Ok(b);
      }
    }
    let fetcher = self.inner.body_fn.lock().await.clone();
    let Some(f) = fetcher else {
      return Err(FerriError::Unsupported(
        "Response.body() is not supported on this backend".into(),
      ));
    };
    let bytes = f().await?;
    self.inner.state.write().await.body_cache = Some(bytes.clone());
    Ok(bytes)
  }

  /// Body decoded as UTF-8 text.
  ///
  /// # Errors
  ///
  /// Returns an error if the body fetch fails or the bytes are not UTF-8.
  pub async fn text(&self) -> Result<String> {
    let bytes = self.body().await?;
    String::from_utf8(bytes).map_err(|e| FerriError::Other(format!("response body is not UTF-8: {e}")))
  }

  /// Body parsed as JSON.
  ///
  /// # Errors
  ///
  /// Returns an error if the body fetch fails or JSON parse fails.
  pub async fn json(&self) -> Result<serde_json::Value> {
    let text = self.text().await?;
    serde_json::from_str(&text).map_err(FerriError::from)
  }

  pub async fn server_addr(&self) -> Option<RemoteAddr> {
    self.inner.state.read().await.remote_addr.clone()
  }

  pub async fn security_details(&self) -> Option<SecurityDetails> {
    self.inner.state.read().await.security_details.clone()
  }

  #[must_use]
  pub fn http_version(&self) -> Option<String> {
    self.inner.http_version.clone()
  }

  // -- Internal mutators ---------------------------------------------------

  pub async fn set_raw_headers(&self, raw: Vec<HeaderEntry>) {
    self.inner.state.write().await.raw_headers = Some(raw);
  }

  pub async fn finish_success(&self) {
    let mut state = self.inner.state.write().await;
    state.finished = Some(Ok(()));
    drop(state);
    self.inner.finished_notify.notify_waiters();
  }

  pub async fn finish_failure(&self, error: String) {
    let mut state = self.inner.state.write().await;
    state.finished = Some(Err(error));
    drop(state);
    self.inner.finished_notify.notify_waiters();
  }
}

pub struct ResponseInit {
  pub request: Request,
  pub url: String,
  pub status: i64,
  pub status_text: String,
  pub from_service_worker: bool,
  pub http_version: Option<String>,
  pub headers: Headers,
  pub remote_addr: Option<RemoteAddr>,
  pub security_details: Option<SecurityDetails>,
  pub body_fn: Option<BodyFn>,
  pub raw_headers_fn: Option<RawHeadersFn>,
}

// ── WebSocket ───────────────────────────────────────────────────────────────

/// WebSocket payload — text frames stay as `String`, binary frames carry bytes.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WebSocketPayload {
  Text(String),
  Binary(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum WebSocketEvent {
  FrameSent(WebSocketPayload),
  FrameReceived(WebSocketPayload),
  Error(String),
  Close,
}

#[derive(Clone)]
pub struct WebSocket {
  inner: Arc<WebSocketState>,
}

impl std::fmt::Debug for WebSocket {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("WebSocket")
      .field("url", &self.inner.url)
      .field("closed", &self.is_closed())
      .finish()
  }
}

pub(crate) struct WebSocketState {
  url: String,
  closed: std::sync::atomic::AtomicBool,
  events: tokio::sync::broadcast::Sender<WebSocketEvent>,
}

impl WebSocket {
  #[must_use]
  pub fn new(url: String) -> Self {
    let (tx, _rx) = tokio::sync::broadcast::channel(256);
    Self {
      inner: Arc::new(WebSocketState {
        url,
        closed: std::sync::atomic::AtomicBool::new(false),
        events: tx,
      }),
    }
  }

  #[must_use]
  pub fn url(&self) -> &str {
    &self.inner.url
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.inner.closed.load(std::sync::atomic::Ordering::Acquire)
  }

  /// Subscribe to frame / error / close events. Receivers see only
  /// events emitted after subscribing — listeners must subscribe before
  /// the activity they care about.
  #[must_use]
  pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<WebSocketEvent> {
    self.inner.events.subscribe()
  }

  // -- Internal mutators ---------------------------------------------------

  pub fn emit_frame_sent(&self, payload: WebSocketPayload) {
    let _ = self.inner.events.send(WebSocketEvent::FrameSent(payload));
  }
  pub fn emit_frame_received(&self, payload: WebSocketPayload) {
    let _ = self.inner.events.send(WebSocketEvent::FrameReceived(payload));
  }
  pub fn emit_error(&self, message: String) {
    let _ = self.inner.events.send(WebSocketEvent::Error(message));
  }
  pub fn emit_close(&self) {
    self.inner.closed.store(true, std::sync::atomic::Ordering::Release);
    let _ = self.inner.events.send(WebSocketEvent::Close);
  }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

#[must_use]
pub fn headers_to_array(map: &Headers) -> Vec<HeaderEntry> {
  map
    .iter()
    .map(|(k, v)| HeaderEntry {
      name: k.clone(),
      value: v.clone(),
    })
    .collect()
}

#[must_use]
pub fn headers_array_to_map(arr: &[HeaderEntry]) -> Headers {
  let mut out: Headers = FxHashMap::default();
  let mut groups: FxHashMap<String, Vec<&str>> = FxHashMap::default();
  for h in arr {
    let lc = h.name.to_ascii_lowercase();
    groups.entry(lc).or_default().push(&h.value);
  }
  for (lc, values) in groups {
    let sep = if lc == "set-cookie" { "\n" } else { ", " };
    out.insert(lc, values.join(sep));
  }
  out
}

#[must_use]
pub fn get_header_value(arr: &[HeaderEntry], name: &str) -> Option<String> {
  let lc = name.to_ascii_lowercase();
  let values: Vec<&str> = arr
    .iter()
    .filter(|h| h.name.to_ascii_lowercase() == lc)
    .map(|h| h.value.as_str())
    .collect();
  if values.is_empty() {
    return None;
  }
  let sep = if lc == "set-cookie" { "\n" } else { ", " };
  Some(values.join(sep))
}

fn url_decode_form(body: &str) -> Vec<(String, String)> {
  body
    .split('&')
    .filter_map(|pair| {
      let mut it = pair.splitn(2, '=');
      let k = it.next()?;
      let v = it.next().unwrap_or("");
      Some((decode(k), decode(v)))
    })
    .collect()
}

fn decode(s: &str) -> String {
  let mut out = Vec::with_capacity(s.len());
  let bytes = s.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    match bytes[i] {
      b'+' => {
        out.push(b' ');
        i += 1;
      },
      b'%' if i + 2 < bytes.len() => {
        if let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
          out.push(hi * 16 + lo);
          i += 3;
        } else {
          out.push(bytes[i]);
          i += 1;
        }
      },
      b => {
        out.push(b);
        i += 1;
      },
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
  match b {
    b'0'..=b'9' => Some(b - b'0'),
    b'a'..=b'f' => Some(b - b'a' + 10),
    b'A'..=b'F' => Some(b - b'A' + 10),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn redirect_chain_links_in_both_directions() {
    let original = Request::new(RequestInit {
      id: "1".into(),
      url: "http://x/a".into(),
      method: "GET".into(),
      resource_type: "Document".into(),
      is_navigation_request: true,
      post_data: None,
      headers: Headers::default(),
      frame_id: None,
      redirected_from: None,
      timing: None,
      raw_headers_fn: None,
    });

    let next = Request::new(RequestInit {
      id: "2".into(),
      url: "http://x/b".into(),
      method: "GET".into(),
      resource_type: "Document".into(),
      is_navigation_request: true,
      post_data: None,
      headers: Headers::default(),
      frame_id: None,
      redirected_from: Some(original.clone()),
      timing: None,
      raw_headers_fn: None,
    });

    assert_eq!(next.redirected_from().unwrap().url(), "http://x/a");
    assert_eq!(original.redirected_to().unwrap().url(), "http://x/b");
  }

  #[test]
  fn headers_array_to_map_joins_set_cookie_with_newlines() {
    let arr = vec![
      HeaderEntry {
        name: "Set-Cookie".into(),
        value: "a=1".into(),
      },
      HeaderEntry {
        name: "Set-Cookie".into(),
        value: "b=2".into(),
      },
      HeaderEntry {
        name: "Content-Type".into(),
        value: "text/plain".into(),
      },
    ];
    let map = headers_array_to_map(&arr);
    assert_eq!(map.get("set-cookie").unwrap(), "a=1\nb=2");
    assert_eq!(map.get("content-type").unwrap(), "text/plain");
  }

  #[test]
  fn ok_status_matches_playwright_semantics() {
    let assert_status = |s: i64, expected: bool| {
      let init = ResponseInit {
        request: Request::new(RequestInit {
          id: "x".into(),
          url: "http://x/".into(),
          method: "GET".into(),
          resource_type: "Document".into(),
          is_navigation_request: true,
          post_data: None,
          headers: Headers::default(),
          frame_id: None,
          redirected_from: None,
          timing: None,
          raw_headers_fn: None,
        }),
        url: "http://x/".into(),
        status: s,
        status_text: String::new(),
        from_service_worker: false,
        http_version: None,
        headers: Headers::default(),
        remote_addr: None,
        security_details: None,
        body_fn: None,
        raw_headers_fn: None,
      };
      let r = Response::new(init);
      assert_eq!(r.ok(), expected, "status {s}");
    };
    assert_status(0, true);
    assert_status(200, true);
    assert_status(204, true);
    assert_status(299, true);
    assert_status(300, false);
    assert_status(404, false);
    assert_status(500, false);
  }
}
