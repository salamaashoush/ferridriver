//! Network lifecycle objects: `Request`, `Response`, `WebSocket`.
//!
//! Mirrors Playwright's client-side `Request` / `Response` / `WebSocket`
//! classes from `packages/playwright-core/src/client/network.ts`. Live
//! object references — listeners hold a `Request` and can call
//! `request.response().await` later; the future resolves when the
//! response (or failure) is recorded by the backend.
//!
//! Ownership is strictly acyclic (`RedirectChain -> RequestState ->
//! ResponseState`) while outer handles (`Request`, `Response`) carry the
//! `Arc<RedirectChain>` so holding any single hop or response keeps the full
//! chain alive and drops everything cleanly once all handles are released.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use rustc_hash::FxHashMap;
use tokio::sync::{Notify, RwLock};

use crate::error::{FerriError, Result};

mod headers;
mod response;
#[cfg(test)]
mod tests;
mod websocket;

pub(crate) use headers::lowercase_names;
use headers::url_decode_form;
pub use headers::{
  get_header_value, headers_array_to_map, headers_map_to_split_array, headers_to_array, known_raw_headers,
};
pub(crate) use response::ResponseState;
pub use response::{Response, ResponseInit};
pub(crate) use websocket::websocket_frame_payload;
pub use websocket::{WebSocket, WebSocketEvent, WebSocketPayload};

/// Header map. Lower-case-keyed combined view (matches Playwright's
/// `headers(): Headers` shape).
pub type Headers = FxHashMap<String, String>;

/// Single header entry preserving original case and duplicate keys.
pub use ferridriver_types::HeaderEntry;

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
  pub(crate) fn empty() -> Self {
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

/// Wall-clock epoch milliseconds (the `RequestTiming::start_time` unit).
pub(crate) fn now_epoch_ms() -> f64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs_f64()
    * 1000.0
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
#[serde(rename_all = "camelCase")]
pub struct RemoteAddr {
  pub ip_address: String,
  pub port: u16,
}

/// TLS certificate info. Resolved from CDP `responseReceived.response.securityDetails`.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityDetails {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub protocol: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub subject_name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub issuer: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub valid_from: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
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

/// Returns a `BodyFn` that reports the operation as unsupported. Kept
/// as a typed fallback for any request whose backend genuinely can't
/// expose its body. (The Playwright `WebKit` backend fetches bodies via
/// `Network.getResponseBody`, so it does not use this.)
#[must_use]
pub fn body_unsupported(reason: &'static str) -> BodyFn {
  Arc::new(move || {
    let reason = reason.to_string();
    Box::pin(async move { Err(FerriError::Unsupported(reason)) })
  })
}

// ── RedirectChain ───────────────────────────────────────────────────────────

/// Acyclic owner for all hops of an HTTP redirect chain.
///
/// Storing the ordered hops in one shared container held by outer [`Request`]
/// handles breaks the doubly-linked `redirected_from <-> redirected_to` `Arc`
/// cycle while guaranteeing that holding any hop keeps the whole chain alive
/// for bidirectional traversal. Non-redirected requests keep `rest` empty
/// (`Vec::new()`, zero heap allocation).
struct RedirectChain {
  first: Arc<RequestState>,
  rest: Mutex<Vec<Arc<RequestState>>>,
}

impl RedirectChain {
  fn single(first: Arc<RequestState>) -> Arc<Self> {
    Arc::new(Self {
      first,
      rest: Mutex::new(Vec::new()),
    })
  }

  fn append(&self, next: Arc<RequestState>) -> usize {
    let mut rest = self.rest.lock().unwrap_or_else(PoisonError::into_inner);
    rest.push(next);
    rest.len()
  }

  fn get(&self, hop: usize) -> Option<Arc<RequestState>> {
    if hop == 0 {
      return Some(Arc::clone(&self.first));
    }
    self
      .rest
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get(hop - 1)
      .cloned()
  }
}

// ── Request ─────────────────────────────────────────────────────────────────

/// Live reference to a network request. Mirrors Playwright's `Request`.
///
/// Cheaply cloneable; every clone shares the same underlying state. Sync
/// getters (`url`, `method`, ...) read immutable fields. Async accessors
/// (`response`, `all_headers`) wait until the backend records the result.
#[derive(Clone)]
pub struct Request {
  pub(super) inner: Arc<RequestState>,
  chain: Arc<RedirectChain>,
  hop: usize,
}

impl std::fmt::Debug for Request {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Request")
      .field("id", &self.inner.id)
      .field("method", &self.inner.method)
      .field("url", &self.inner.url)
      .field("resource_type", &self.inner.resource_type)
      .finish_non_exhaustive()
  }
}

/// Sync-readable mutable metadata stored inline inside [`RequestState`] so
/// timing/size/failure updates require no separate heap `Arc` allocations.
struct RequestSyncState {
  timing: RequestTiming,
  sizes: RequestSizes,
  failure: Option<String>,
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
  /// Stable identity of the page whose listener captured this request
  /// (CDP `targetId` / `WebKit` `pageProxyId` / `BiDi` top-level context
  /// id). Stamped at capture time so context-level consumers (HAR
  /// `pages`/`pageref`, page-scoped `routeFromHAR({update})`) can
  /// attribute entries without a frame-to-page reverse lookup.
  page_guid: Option<String>,

  // Sync-readable mutable fields (`timing()`, `sizes()`, `failure()`).
  sync: Mutex<RequestSyncState>,

  // Async-only mutable state (guarded by `state`).
  state: RwLock<RequestMutState>,

  // Wakers — `notify_waiters()` on every state transition.
  outcome_notify: Notify,
  headers_notify: Notify,

  // Async fetcher (immutable after construction).
  raw_headers_fn: Option<RawHeadersFn>,
}

struct RequestMutState {
  raw_headers: Option<Vec<HeaderEntry>>,
  response: Option<Arc<ResponseState>>,
}

impl Request {
  /// Construct a fresh `Request`. Called by backend listeners on the
  /// initial `requestWillBeSent` / `beforeRequestSent` event. When
  /// `init.redirected_from` is set, the constructor appends this hop to the
  /// prior request's [`RedirectChain`] synchronously.
  #[must_use]
  pub fn new(init: RequestInit) -> Self {
    let timing = init.timing.unwrap_or_else(|| RequestTiming {
      // Wall-clock anchor at capture time — backends without a
      // wall-time sample in their request event still satisfy the
      // epoch-ms `start_time` contract.
      start_time: now_epoch_ms(),
      ..RequestTiming::empty()
    });
    let inner = Arc::new(RequestState {
      id: init.id,
      url: without_fragment(init.url),
      method: init.method,
      resource_type: init.resource_type,
      is_navigation_request: init.is_navigation_request,
      post_data: init.post_data,
      provisional_headers: init.headers,
      frame_id: init.frame_id,
      page_guid: init.page_guid,
      sync: Mutex::new(RequestSyncState {
        timing,
        sizes: RequestSizes::default(),
        failure: None,
      }),
      state: RwLock::new(RequestMutState {
        raw_headers: None,
        response: None,
      }),
      outcome_notify: Notify::new(),
      headers_notify: Notify::new(),
      raw_headers_fn: init.raw_headers_fn,
    });

    let (chain, hop) = match init.redirected_from {
      Some(prev) => {
        let hop = prev.chain.append(Arc::clone(&inner));
        (prev.chain, hop)
      },
      None => (RedirectChain::single(Arc::clone(&inner)), 0),
    };

    Self { inner, chain, hop }
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
  /// Whether both handles are the one request: a redirect's next hop is
  /// another request under the same protocol id.
  pub(crate) fn same_request(&self, other: &Request) -> bool {
    Arc::ptr_eq(&self.inner, &other.inner)
  }

  #[must_use]
  pub fn frame_id(&self) -> Option<&str> {
    self.inner.frame_id.as_deref()
  }

  /// Stable capture-time page identity — see the `RequestState` field.
  #[must_use]
  pub fn page_guid(&self) -> Option<&str> {
    self.inner.page_guid.as_deref()
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
    if self
      .inner
      .provisional_headers
      .iter()
      .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("application/x-www-form-urlencoded"))
    {
      let mut entries = serde_json::Map::new();
      for (k, v) in url_decode_form(&body) {
        entries.insert(k, serde_json::Value::String(v));
      }
      return Ok(Some(serde_json::Value::Object(entries)));
    }
    serde_json::from_str(&body)
      .map(Some)
      .map_err(|_| FerriError::Backend(format!("POST data is not a valid JSON object: {body}")))
  }

  /// Provisional headers (matches Playwright's deprecated `headers()`).
  /// For the post-extraInfo headers use `all_headers()`.
  #[must_use]
  pub fn headers(&self) -> Headers {
    lowercase_names(&self.inner.provisional_headers)
  }

  /// Headers as `[{name, value}]` preserving order and duplicates.
  ///
  /// Playwright's `headersArray()`, which resolves the ACTUAL headers
  /// (`_actualHeaders()` in `client/network.ts`) rather than the
  /// provisional ones -- duplicates only survive there. Falls back to
  /// the provisional list when the backend cannot produce them.
  pub async fn headers_array(&self) -> Vec<HeaderEntry> {
    self
      .fetch_raw_headers()
      .await
      .unwrap_or_else(|_| headers_to_array(&self.inner.provisional_headers))
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
    if let Some(f) = &self.inner.raw_headers_fn {
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

  // -- Redirect chain ------------------------------------------------------

  #[must_use]
  pub fn redirected_from(&self) -> Option<Request> {
    let prev_hop = self.hop.checked_sub(1)?;
    let inner = self.chain.get(prev_hop)?;
    Some(Request {
      inner,
      chain: Arc::clone(&self.chain),
      hop: prev_hop,
    })
  }

  /// Sync — Playwright's `Request.redirectedTo(): Request | null`.
  #[must_use]
  pub fn redirected_to(&self) -> Option<Request> {
    let next_hop = self.hop + 1;
    let inner = self.chain.get(next_hop)?;
    Some(Request {
      inner,
      chain: Arc::clone(&self.chain),
      hop: next_hop,
    })
  }

  // -- Outcome -------------------------------------------------------------

  /// Failure text, if the request failed before producing a response.
  /// Sync — mirrors Playwright's `request.failure()` getter.
  #[must_use]
  pub fn failure(&self) -> Option<String> {
    self
      .inner
      .sync
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .failure
      .clone()
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
        if let Some(r) = state.response.as_ref() {
          return Ok(Some(Response::from_parts(self.clone(), Arc::clone(r))));
        }
        if self
          .inner
          .sync
          .lock()
          .unwrap_or_else(PoisonError::into_inner)
          .failure
          .is_some()
        {
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
      .as_ref()
      .map(|r| Response::from_parts(self.clone(), Arc::clone(r)))
  }

  // -- Timing / sizes ------------------------------------------------------

  /// Sync — Playwright's `Request.timing(): ResourceTiming`.
  #[must_use]
  pub fn timing(&self) -> RequestTiming {
    self.inner.sync.lock().unwrap_or_else(PoisonError::into_inner).timing
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
      return Err(FerriError::Backend("Unable to fetch sizes for failed request".into()));
    }
    Ok(self.inner.sync.lock().unwrap_or_else(PoisonError::into_inner).sizes)
  }

  // -- Internal mutators (called by backend listeners) ---------------------

  /// Record raw headers from `requestWillBeSentExtraInfo` / equivalent.
  pub async fn set_raw_headers(&self, raw: Vec<HeaderEntry>) {
    let mut state = self.inner.state.write().await;
    state.raw_headers = Some(raw);
    drop(state);
    self.inner.headers_notify.notify_waiters();
  }

  /// Merge a backend `ResourceTiming` sample. `start_time` keeps the
  /// wall-clock epoch-ms anchor captured at request creation — protocol
  /// samples carry a monotonic `requestTime` that must not overwrite it.
  pub fn update_timing(&self, timing: RequestTiming) {
    let mut sync = self.inner.sync.lock().unwrap_or_else(PoisonError::into_inner);
    let existing = sync.timing.start_time;
    sync.timing = if existing > 0.0 {
      RequestTiming {
        start_time: existing,
        ..timing
      }
    } else {
      timing
    };
  }

  pub fn update_sizes(&self, sizes: RequestSizes) {
    self.inner.sync.lock().unwrap_or_else(PoisonError::into_inner).sizes = sizes;
  }

  pub async fn set_response(&self, response: &Response) {
    let mut state = self.inner.state.write().await;
    state.response = Some(Arc::clone(&response.inner));
    drop(state);
    self.inner.outcome_notify.notify_waiters();
  }

  pub fn set_failure(&self, error_text: String) {
    self.inner.sync.lock().unwrap_or_else(PoisonError::into_inner).failure = Some(error_text);
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
        .and_then(|r| lowercase_names(&r.provisional_headers).remove("content-type")),
      "headers": self.inner.provisional_headers,
      "postData": self.post_data(),
      "failure": self.failure(),
    })
  }

  /// Compact JSON snapshot: method, status, and URL only.
  ///
  /// The full [`Self::to_diagnostic_json`] record carries every request and
  /// response header, which on a real page adds up to hundreds of kilobytes
  /// across the log. Most questions asked of the network log ("where did this
  /// asset load from", "did this call 404") need only these three fields.
  pub async fn to_summary_json(&self) -> serde_json::Value {
    let state = self.inner.state.read().await;
    serde_json::json!({
      "method": self.inner.method,
      "status": state.response.as_ref().map(|r| r.status),
      "url": self.inner.url,
      "failure": self.failure(),
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
  pub page_guid: Option<String>,
  pub redirected_from: Option<Request>,
  pub timing: Option<RequestTiming>,
  pub raw_headers_fn: Option<RawHeadersFn>,
}

/// A request never carries its URL's fragment, though `WebKit` and `BiDi`
/// report it on a navigation (Playwright's `stripFragmentFromUrl`,
/// `server/network.ts`). Routes match and `url()` reads without it.
pub(crate) fn without_fragment(mut url: String) -> String {
  if let Some(hash) = url.find('#') {
    url.truncate(hash);
  }
  url
}

/// How long a navigation waits for its document request to be seen after
/// the lifecycle says the navigation finished. Long enough to cover the
/// gap between two event consumers, short enough that a navigation with
/// no request at all (same-document, back/forward cache) is not held up.
pub const NAV_REQUEST_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// A navigation request and the document loader it belongs to.
type LoaderRequest = (Option<String>, Request);

/// Shared slot for the most recent main-document navigation `Request`.
///
/// Filled by backend network listeners whenever a request with
/// `is_navigation_request == true` is observed (matches Playwright's
/// `_isNavigationRequest` flag — `requestId == loaderId` on CDP,
/// `navigation` field present on `BiDi`). Consumed by
/// `AnyPage::goto` / `reload` / `go_back` / `go_forward` to resolve
/// the final main-document `Response` after the lifecycle waiter
/// fires. Callers clear the slot before issuing the navigation so
/// same-document navigations (no new request) can be detected as
/// "no response" per Playwright's `Response | null` contract.
#[derive(Clone, Default)]
pub struct NavRequestSlot {
  /// Every navigation request since the last clear, oldest first, with
  /// the loader it belongs to where the backend reports one.
  inner: Arc<Mutex<Vec<LoaderRequest>>>,
  /// The loader the main frame last committed, for a waiter that did not
  /// learn its own: a child frame's document request comes after the
  /// page's and is a navigation request too.
  committed: Arc<Mutex<Option<String>>>,
  filled: Arc<Notify>,
}

impl NavRequestSlot {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  pub fn set(&self, request: Request) {
    self.push(None, request);
  }

  /// [`Self::set`] for a request whose document loader is known. Starting
  /// a navigation cancels the one in flight, whose request can still be
  /// reported after the slot was cleared for the new one; the loader is
  /// what tells them apart.
  pub fn set_for_loader(&self, request: Request, loader_id: String) {
    self.push(Some(loader_id), request);
  }

  fn push(&self, loader_id: Option<String>, request: Request) {
    if let Ok(mut guard) = self.inner.lock() {
      guard.push((loader_id, request));
    }
    self.filled.notify_waiters();
  }

  /// Record the loader of a main-frame commit.
  pub fn commit_loader(&self, loader_id: String) {
    if let Ok(mut guard) = self.committed.lock() {
      *guard = Some(loader_id);
    }
    self.filled.notify_waiters();
  }

  #[must_use]
  pub fn get(&self) -> Option<Request> {
    self.get_for(None)
  }

  /// The latest request of the navigation under `loader_id`; without one,
  /// of the loader the main frame committed, or the latest of any before a
  /// commit. A request reported without a loader matches every loader.
  #[must_use]
  pub fn get_for(&self, loader_id: Option<&str>) -> Option<Request> {
    let committed = self.committed.lock().ok().and_then(|guard| guard.clone());
    let loader_id = loader_id.or(committed.as_deref());
    let guard = self.inner.lock().ok()?;
    guard
      .iter()
      .rev()
      .find(|(loader, _)| loader_id.is_none() || loader.is_none() || loader.as_deref() == loader_id)
      .map(|(_, request)| request.clone())
  }

  /// The navigation request, waiting up to `timeout` for one to land.
  /// A caller that names its loader is answered as soon as the slot has
  /// seen that loader commit: a document with no request at all
  /// (`about:blank`, a back/forward-cache restore) then costs nothing.
  ///
  /// The lifecycle events a navigation waiter watches and the network
  /// events that fill this slot reach us on separate consumers, so a
  /// document request that went out FIRST can still be processed after
  /// the `load` that followed it — reading the slot once made
  /// `goBack()` return `None` for a navigation that plainly had a
  /// response. A navigation that genuinely issues no request (a
  /// same-document jump, a back/forward-cache restore) costs the wait
  /// and then answers `None` as before.
  pub async fn wait(&self, timeout: std::time::Duration) -> Option<Request> {
    self.wait_for(None, timeout).await
  }

  async fn wait_for(&self, loader_id: Option<&str>, timeout: std::time::Duration) -> Option<Request> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
      let notified = self.filled.notified();
      tokio::pin!(notified);
      // Arm before the look: a `set` racing between the two would
      // otherwise notify nobody and leave this waiting out the timeout.
      notified.as_mut().enable();
      // Read before the requests: the consumer that fills this slot sees
      // a document's request before its commit, so a commit already
      // recorded means every request of that loader is in the slot.
      let settled = loader_id.is_some() && self.committed_loader().as_deref() == loader_id;
      if let Some(request) = self.get_for(loader_id) {
        return Some(request);
      }
      if settled || tokio::time::timeout_at(deadline, notified).await.is_err() {
        return self.get_for(loader_id);
      }
    }
  }

  fn committed_loader(&self) -> Option<String> {
    self.committed.lock().ok().and_then(|guard| guard.clone())
  }

  pub fn clear(&self) {
    if let Ok(mut guard) = self.inner.lock() {
      guard.clear();
    }
    if let Ok(mut guard) = self.committed.lock() {
      *guard = None;
    }
  }

  pub async fn final_response(&self, grace: std::time::Duration) -> Option<Response> {
    self.final_response_for(None, grace).await
  }

  /// [`Self::final_response`] of the navigation under `loader_id`.
  pub async fn final_response_for(&self, loader_id: Option<&str>, grace: std::time::Duration) -> Option<Response> {
    let mut request = self.wait_for(loader_id, grace).await?;
    loop {
      let changed = self.filled.notified();
      tokio::pin!(changed);
      changed.as_mut().enable();
      while let Some(next) = request.redirected_to() {
        request = next;
      }
      let response = request.response().await.ok().flatten()?;
      if !matches!(response.status(), 301 | 302 | 303 | 307 | 308) || !response.headers().contains_key("location") {
        return Some(response);
      }
      // Navigation results and network events have separate consumers; the
      // final redirect hop can still be queued when the lifecycle resolves.
      tokio::time::timeout(grace, changed).await.ok()?;
      request = self.get_for(loader_id)?;
    }
  }
}
