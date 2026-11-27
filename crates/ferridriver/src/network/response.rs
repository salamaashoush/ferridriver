//! Live HTTP `Response` handle and body/header caching.

use std::sync::Arc;

use tokio::sync::{Notify, RwLock};

use crate::error::{FerriError, Result};

use super::headers::{get_header_value, headers_array_to_map, headers_to_array, lowercase_names};
use super::{BodyFn, HeaderEntry, Headers, RawHeadersFn, RemoteAddr, Request, SecurityDetails, without_fragment};

/// Live reference to an HTTP response. Mirrors Playwright's `Response`.
///
/// Holds both the originating [`Request`] handle and the response's own
/// `ResponseState` rather than storing an `Arc<RequestState>` back-pointer
/// inside `ResponseState`. That breaks the `RequestState <-> ResponseState`
/// reference cycle while preserving Playwright's lifetime guarantee: holding
/// either a `Request` or a `Response` keeps both sides alive.
#[derive(Clone)]
pub struct Response {
  request: Request,
  pub(super) inner: Arc<ResponseState>,
}

impl std::fmt::Debug for Response {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Response")
      .field("status", &self.inner.status)
      .field("status_text", &self.inner.status_text)
      .field("url", &self.inner.url)
      .finish_non_exhaustive()
  }
}

pub(crate) struct ResponseState {
  pub(super) url: String,
  pub(super) status: i64,
  pub(super) status_text: String,
  pub(super) from_service_worker: bool,
  pub(super) provisional_headers: Headers,

  state: RwLock<ResponseMutState>,
  finished_notify: Notify,

  body_fn: Option<BodyFn>,
  raw_headers_fn: Option<RawHeadersFn>,
}

struct ResponseMutState {
  raw_headers: Option<Vec<HeaderEntry>>,
  body_cache: Option<Vec<u8>>,
  remote_addr: Option<RemoteAddr>,
  security_details: Option<SecurityDetails>,
  /// HTTP protocol version reported by the backend (e.g. `http/1.1`).
  /// Captured at construction; lives in the async state alongside the
  /// other backend-reported metadata so the public accessor mirrors
  /// Playwright's `Promise<string>` shape.
  http_version: Option<String>,
  finished: Option<std::result::Result<(), FerriError>>,
}

impl Response {
  pub(super) fn from_parts(request: Request, inner: Arc<ResponseState>) -> Self {
    Self { request, inner }
  }

  #[must_use]
  pub fn new(init: ResponseInit) -> Self {
    let inner = Arc::new(ResponseState {
      url: without_fragment(init.url),
      status: init.status,
      status_text: init.status_text,
      from_service_worker: init.from_service_worker,
      provisional_headers: init.headers,
      state: RwLock::new(ResponseMutState {
        raw_headers: None,
        body_cache: None,
        remote_addr: init.remote_addr,
        security_details: init.security_details,
        http_version: init.http_version,
        finished: None,
      }),
      finished_notify: Notify::new(),
      body_fn: init.body_fn,
      raw_headers_fn: init.raw_headers_fn,
    });
    Self {
      request: init.request,
      inner,
    }
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
    self.request.clone()
  }
  #[must_use]
  pub fn frame_id(&self) -> Option<&str> {
    self.request.frame_id()
  }

  #[must_use]
  pub fn headers(&self) -> Headers {
    lowercase_names(&self.inner.provisional_headers)
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

  /// See [`Request::headers_array`]: the actual headers, so a header the
  /// server sent twice is two entries.
  pub async fn headers_array(&self) -> Vec<HeaderEntry> {
    self
      .fetch_raw_headers()
      .await
      .unwrap_or_else(|_| headers_to_array(&self.inner.provisional_headers))
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
    Ok(
      raw
        .iter()
        .filter(|h| h.name.eq_ignore_ascii_case(name))
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
    if let Some(f) = &self.inner.raw_headers_fn {
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
  pub async fn finished(&self) -> std::result::Result<(), FerriError> {
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

  /// Whether the body finished loading or failed, without waiting.
  pub(crate) async fn is_finished(&self) -> bool {
    self.inner.state.read().await.finished.is_some()
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
    let Some(f) = &self.inner.body_fn else {
      return Err(FerriError::Unsupported(
        "Response.body() is not supported on this backend".into(),
      ));
    };
    // Playwright's `internalBody` awaits the response finishing first: a
    // body read as soon as a `commit` goto returns is still arriving, and
    // Chromium has none to give for it yet.
    let _ = self.finished().await;
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
    String::from_utf8(bytes).map_err(|e| FerriError::Backend(format!("response body is not UTF-8: {e}")))
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

  /// Playwright: `response.serverAddr()`. Some backends learn the address
  /// only once the body has arrived, so this waits for the response to
  /// finish, as Playwright's does.
  pub async fn server_addr(&self) -> Option<RemoteAddr> {
    let _ = self.finished().await;
    self.inner.state.read().await.remote_addr.clone()
  }

  /// Playwright: `response.securityDetails()`. Waits for the response to
  /// finish, as Playwright's does.
  pub async fn security_details(&self) -> Option<SecurityDetails> {
    let _ = self.finished().await;
    self.inner.state.read().await.security_details.clone()
  }

  /// HTTP protocol version already observed on this response, without
  /// waiting for the response to finish. Used by HAR/trace flushing so an
  /// open streaming response (such as SSE) is recorded immediately.
  pub async fn http_version_now(&self) -> Option<String> {
    self.inner.state.read().await.http_version.clone()
  }

  /// HTTP protocol version (e.g. `http/1.1`), or `None` when the
  /// backend did not report one. Playwright: `response.httpVersion():
  /// Promise<string>` (`client/network.ts`) — async to mirror the
  /// channel round-trip Playwright performs. Waits for the response to
  /// finish when the version arrives on loading-finished metrics (`WebKit`).
  pub async fn http_version(&self) -> Option<String> {
    let version = self.http_version_now().await;
    if version.is_some() {
      return version;
    }
    let _ = self.finished().await;
    self.http_version_now().await
  }

  // -- Internal mutators ---------------------------------------------------

  pub async fn set_raw_headers(&self, raw: Vec<HeaderEntry>) {
    self.inner.state.write().await.raw_headers = Some(raw);
  }

  pub async fn set_server_addr(&self, addr: RemoteAddr) {
    self.inner.state.write().await.remote_addr = Some(addr);
  }

  pub async fn set_http_version(&self, protocol: String) {
    if !protocol.is_empty() {
      self.inner.state.write().await.http_version = Some(protocol);
    }
  }

  pub async fn set_security_protocol(&self, protocol: String) {
    if protocol.is_empty() {
      return;
    }
    let mut state = self.inner.state.write().await;
    if let Some(details) = state.security_details.as_mut() {
      details.protocol = Some(protocol);
    }
  }

  pub async fn finish_success(&self) {
    let mut state = self.inner.state.write().await;
    state.finished = Some(Ok(()));
    drop(state);
    self.inner.finished_notify.notify_waiters();
  }

  pub async fn finish_failure(&self, error: impl Into<FerriError>) {
    let mut state = self.inner.state.write().await;
    state.finished = Some(Err(error.into()));
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
