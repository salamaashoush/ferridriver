//! The runner-side HTTP client — the Playwright `request` adapter over
//! the `ferrijs_fetch` engine, separate from the browser/page network.
//! Backs both the `fetch` global and the Playwright-style `request`
//! binding, which lower into the same [`fetch::Request`] and go through
//! the same [`fetch::send`] path.
//!
//! [`HttpClient`] provides `get`, `post`, `put`, `delete`, `patch`,
//! `head`, and generic `fetch` (buffered) / `fetch_stream` (streamed).
//! Each buffered call returns an [`HttpResponse`] with `status()`,
//! `text()`, `json()`, `headers()`, `ok()`, and `body()`.
//!
//! ```ignore
//! let ctx = HttpClient::new(HttpClientOptions {
//!     base_url: Some("https://api.example.com".into()),
//!     ..Default::default()
//! });
//! let resp = ctx.get("/users", None).await?;
//! assert!(resp.ok());
//! let users: Vec<User> = resp.json()?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;

use ferrijs_fetch as fetch;

use crate::options::{HttpCredentials, HttpCredentialsSend};

pub use ferrijs_fetch::{
  BridgeFuture, ContextBridge, ContextDefaults, Credentials, FetchError, MultipartField, MultipartValue, NetGuard,
  NetPolicy, RedirectMode, RemoteAddr, ResponseType, multipart_boundary, multipart_boundary_of, parse_multipart,
  serialize_multipart,
};

/// Options for creating an `HttpClient`.
#[derive(Debug, Clone, Default)]
pub struct HttpClientOptions {
  /// Base URL prepended to relative paths (e.g., `"https://api.example.com"`).
  pub base_url: Option<String>,
  /// Default headers sent with every request.
  pub extra_http_headers: Vec<(String, String)>,
  /// Default timeout per request.
  pub timeout: Option<Duration>,
  /// Ignore HTTPS certificate errors.
  pub ignore_https_errors: bool,
  /// Playwright `httpCredentials`: what answers a Basic challenge, the
  /// first entry bound to no origin or to the request's.
  pub http_credentials: Vec<HttpCredentials>,
}

/// The credentials a context-bound client answers challenges with, read
/// when one is needed so `setHTTPCredentials` after the client was made
/// still counts.
pub type LiveHttpCredentials = Arc<dyn Fn() -> futures::future::BoxFuture<'static, Vec<HttpCredentials>> + Send + Sync>;

#[derive(Clone)]
enum CredentialSource {
  Fixed(Vec<HttpCredentials>),
  Live(LiveHttpCredentials),
}

fn basic_authorization(credentials: &HttpCredentials) -> String {
  use base64::Engine as _;
  let pair = format!("{}:{}", credentials.username, credentials.password);
  format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
}

/// Per-request options (overrides context defaults). This is the
/// Playwright `request` option bag; it is lowered into a
/// [`fetch::Request`] before the engine sees it.
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
  /// Override HTTP method (normally set by the convenience method).
  pub method: Option<String>,
  /// Extra headers for this request.
  pub headers: Option<Vec<(String, String)>>,
  /// Raw request body.
  pub data: Option<Vec<u8>>,
  /// JSON request body (serialized automatically, sets Content-Type).
  pub json_data: Option<serde_json::Value>,
  /// URL-encoded form data.
  pub form: Option<Vec<(String, String)>>,
  /// `multipart/form-data` body (files + text fields). Mutually
  /// exclusive with `data`/`json_data`/`form`; serialized to a body +
  /// boundary content-type before send.
  pub multipart: Option<Vec<MultipartField>>,
  /// Query string parameters.
  pub params: Option<Vec<(String, String)>>,
  /// Per-request timeout override.
  pub timeout: Option<Duration>,
  /// Fail with error on 4xx/5xx status codes.
  pub fail_on_status_code: Option<bool>,
  /// WHATWG redirect handling. `Follow` (default) uses `max_redirects`
  /// as the cap; `Manual` returns the 3xx unfollowed; `Error` treats a
  /// redirect as a network error.
  pub redirect: RedirectMode,
  /// Per-request redirect cap for `RedirectMode::Follow`: `Some(0)` does
  /// not follow, `Some(n)` follows up to `n` then errors, `None` uses
  /// the engine default (20).
  pub max_redirects: Option<u32>,
  /// Retry the request on a connection reset up to this many times
  /// (exponential backoff), mirroring Playwright's `maxRetries`. `None`
  /// or `Some(0)` = no retry.
  pub max_retries: Option<u32>,
  /// Per-request override of the client-level `ignore_https_errors`.
  /// `None` = inherit the client (or context) default.
  pub ignore_https_errors: Option<bool>,
  /// WHATWG `credentials` mode. `None` = the default (`SameOrigin` —
  /// send stored cookies); `Some(Omit)` sends no cookies and bypasses
  /// the cookie jar entirely.
  pub credentials: Option<Credentials>,
  /// Sandbox network policy. `None`/inert ⇒ the unguarded fast path.
  /// `Some(active)` enforces the allow-list + metadata/private/scheme
  /// rules on the initial URL, every redirect hop, and every resolved
  /// address.
  pub net_guard: Option<NetGuard>,
}

/// A request assembled by the WHATWG `fetch` layer, for
/// [`HttpClient::fetch_whatwg`].
///
/// Unlike [`RequestOptions`] this carries no body *forms* — the spec's
/// "extract a body" step has already run, so `body` is final and
/// `headers` already state its `content-type` (or deliberately state
/// none).
pub struct WhatwgRequest {
  /// Absolute, or relative to the client's `baseURL`.
  pub url: String,
  pub method: String,
  /// Fully assembled request headers, applied over the client/context
  /// defaults.
  pub headers: Vec<(String, String)>,
  pub body: fetch::Body,
  pub redirect: RedirectMode,
  pub credentials: Credentials,
  /// Sandbox network policy (`allow.net`, metadata blocking).
  pub net_guard: Option<NetGuard>,
  /// `None` uses the client default.
  pub timeout: Option<Duration>,
}

/// Per-request defaults resolved from the browser context (when bound)
/// or the client options.
struct ResolvedDefaults {
  base_url: Option<String>,
  extra_headers: Vec<(String, String)>,
  user_agent: Option<String>,
  ignore_https_errors: bool,
  /// Whether to seed Playwright's context-style `user-agent` + `accept`
  /// base headers.
  ctx_style: bool,
}

/// A buffered HTTP response (the Playwright `APIResponse` view over a
/// [`fetch::Response`]): sync `status()`/`ok()`/`headers()`, async body
/// already collected.
#[derive(Debug, Clone)]
pub struct HttpResponse {
  status_code: u16,
  status_text: String,
  response_url: String,
  response_headers: fetch::Headers,
  body_bytes: bytes::Bytes,
  server_addr: Option<RemoteAddr>,
  redirected: bool,
  unfollowed_redirect: bool,
  response_type: ResponseType,
  timing: crate::network::RequestTiming,
  peer_certificate: Option<Vec<u8>>,
  disposed: bool,
}

/// Playwright's `ResourceTiming` for the hop that produced a response
/// (`server/fetch.ts::notifyRequestFinished`): milliseconds from
/// `start_time`, `-1` for a phase that did not happen. The transport
/// reports when a connection was ready and not when its TLS handshake
/// began, so `secure_connection_start` is always `-1`.
fn resource_timing(timing: &fetch::Timing, ended: std::time::SystemTime) -> crate::network::RequestTiming {
  let ms = |offset: Duration| offset.as_secs_f64() * 1000.0;
  let since_epoch = |time: std::time::SystemTime| {
    time
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
  };
  let start_time = since_epoch(timing.start_time);
  crate::network::RequestTiming {
    // A wall-clock reading in whole milliseconds, as `Date.now()` gives
    // Playwright's; the offsets below keep their fractions.
    start_time: start_time.floor(),
    domain_lookup_start: timing.dns_end.map_or(-1.0, |_| 0.0),
    domain_lookup_end: timing.dns_end.map_or(-1.0, ms),
    connect_start: timing.connect_end.map_or(-1.0, |_| timing.dns_end.map_or(0.0, ms)),
    secure_connection_start: -1.0,
    connect_end: timing.connect_end.map_or(-1.0, ms),
    // A connection the pool already had is written to at once.
    request_start: timing.connect_end.map_or(0.0, ms),
    response_start: ms(timing.response_start),
    response_end: (since_epoch(ended) - start_time).max(ms(timing.response_start)),
  }
}

/// Playwright's `SecurityDetails` from the server's leaf certificate
/// (`server/fetch.ts::captureSecurityDetails`): the first common name of
/// the subject and of the issuer, and the validity window in seconds.
/// The transport does not report the negotiated protocol version.
fn security_details(certificate: &[u8]) -> Option<crate::network::SecurityDetails> {
  let (_, certificate) = x509_parser::parse_x509_certificate(certificate).ok()?;
  let common_name = |name: &x509_parser::x509::X509Name<'_>| {
    name
      .iter_common_name()
      .next()
      .and_then(|cn| cn.as_str().ok())
      .map(ToString::to_string)
  };
  // Certificate dates are whole seconds, well inside what `f64` holds.
  #[allow(clippy::cast_precision_loss)]
  let seconds = |time: x509_parser::time::ASN1Time| time.timestamp() as f64;
  Some(crate::network::SecurityDetails {
    protocol: None,
    subject_name: common_name(certificate.subject()),
    issuer: common_name(certificate.issuer()),
    valid_from: Some(seconds(certificate.validity().not_before)),
    valid_to: Some(seconds(certificate.validity().not_after)),
  })
}

impl HttpResponse {
  /// Buffer a [`fetch::Response`] into an `HttpResponse`.
  async fn buffer(response: fetch::Response) -> crate::error::Result<Self> {
    let fetch::Response {
      status,
      status_text,
      url,
      headers,
      body,
      redirected,
      unfollowed_redirect,
      server_addr,
      type_,
      timing,
      peer_certificate,
    } = response;
    let body_bytes = body.collect().await?;
    Ok(Self {
      status_code: status,
      status_text,
      response_url: url,
      response_headers: headers,
      body_bytes,
      server_addr,
      redirected,
      unfollowed_redirect,
      response_type: type_,
      timing: resource_timing(&timing, std::time::SystemTime::now()),
      peer_certificate,
      disposed: false,
    })
  }

  /// Playwright: `apiResponse.timing(): ResourceTiming`, the phases of
  /// the request that produced this response.
  #[must_use]
  pub fn timing(&self) -> crate::network::RequestTiming {
    self.timing
  }

  /// Playwright: `apiResponse.securityDetails()`. `None` for a response
  /// that did not arrive over TLS.
  #[must_use]
  pub fn security_details(&self) -> Option<crate::network::SecurityDetails> {
    self.peer_certificate.as_deref().and_then(security_details)
  }

  /// HTTP status code.
  #[must_use]
  pub fn status(&self) -> u16 {
    self.status_code
  }

  /// HTTP status text (e.g., "OK", "Not Found").
  #[must_use]
  pub fn status_text(&self) -> &str {
    &self.status_text
  }

  /// Final URL after redirects.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.response_url
  }

  /// Whether the response status is 200-299.
  #[must_use]
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status_code)
  }

  /// Response headers as (name, value) pairs, verbatim: duplicates and
  /// original casing preserved (Playwright's `headersArray()`).
  #[must_use]
  pub fn headers(&self) -> &[(String, String)] {
    self.response_headers.entries()
  }

  /// Flattened header object: lowercased names, combined values
  /// (Playwright's `headers()`).
  #[must_use]
  pub fn headers_object(&self) -> Vec<(String, String)> {
    self.response_headers.to_object()
  }

  /// Combined value of a header (case-insensitive), duplicates joined
  /// with `, ` — `\n` for `set-cookie`. Playwright's `RawHeaders.get`.
  #[must_use]
  pub fn header(&self, name: &str) -> Option<String> {
    self.response_headers.get(name)
  }

  /// Response body as UTF-8 string.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid UTF-8, or if the response
  /// was disposed.
  pub fn text(&self) -> crate::error::Result<String> {
    String::from_utf8(self.body()?.to_vec())
      .map_err(|e| crate::error::FerriError::evaluation(format!("response body is not UTF-8: {e}")))
  }

  /// Parse response body as JSON.
  ///
  /// # Errors
  ///
  /// Returns an error if the body cannot be deserialized, or if the
  /// response was disposed.
  pub fn json<T: serde::de::DeserializeOwned>(&self) -> crate::error::Result<T> {
    serde_json::from_slice(self.body()?).map_err(Into::into)
  }

  /// Response body as a JSON value.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid JSON, or if the response
  /// was disposed.
  pub fn json_value(&self) -> crate::error::Result<serde_json::Value> {
    self.json()
  }

  /// Raw response body bytes.
  ///
  /// # Errors
  ///
  /// Returns an error if the response was disposed.
  pub fn body(&self) -> crate::error::Result<&[u8]> {
    if self.disposed {
      return Err(crate::error::FerriError::Disposed("Response"));
    }
    Ok(&self.body_bytes)
  }

  /// The body as a refcounted [`bytes::Bytes`] — a cheap clone that
  /// shares the buffer rather than copying it.
  ///
  /// Lets a binding hand the body to its runtime without a copy (the
  /// `QuickJS` layer builds an `ArrayBuffer` directly over this
  /// allocation). Prefer [`Self::body`] when a plain slice will do.
  ///
  /// # Errors
  ///
  /// Returns an error if the response was disposed.
  pub fn body_shared(&self) -> crate::error::Result<bytes::Bytes> {
    if self.disposed {
      return Err(crate::error::FerriError::Disposed("Response"));
    }
    Ok(self.body_bytes.clone())
  }

  /// Resolved peer address (`{ ipAddress, port }`), or `None` when the
  /// transport didn't surface one. Playwright:
  /// `apiResponse.serverAddr(): Promise<RemoteAddr | null>`.
  #[must_use]
  pub fn server_addr(&self) -> Option<&RemoteAddr> {
    self.server_addr.as_ref()
  }

  /// Whether at least one redirect hop was followed (WHATWG
  /// `Response.redirected`).
  #[must_use]
  pub fn redirected(&self) -> bool {
    self.redirected
  }

  /// Whether this 3xx was returned without following because
  /// `redirect: manual` was requested — the JS layer maps it to an
  /// opaque-redirect `Response`.
  #[must_use]
  pub fn unfollowed_redirect(&self) -> bool {
    self.unfollowed_redirect
  }

  /// WHATWG `Response.type` (basic / opaqueredirect / …).
  #[must_use]
  pub fn response_type(&self) -> ResponseType {
    self.response_type
  }

  /// Playwright: `apiResponse.dispose()` — release the buffered body.
  /// Status, headers and URL stay readable; `text()`/`json()`/`body()`
  /// then fail with [`FerriError::Disposed`](crate::error::FerriError).
  pub fn dispose(&mut self) {
    self.body_bytes = bytes::Bytes::new();
    self.disposed = true;
  }
}

/// A response whose body has NOT been buffered: status/headers are
/// available immediately, body bytes are pulled incrementally with
/// [`Self::chunk`]. Produced by [`HttpClient::fetch_stream`]; backs a
/// WHATWG `Response.body` `ReadableStream`.
pub struct HttpStreamResponse {
  status_code: u16,
  status_text: String,
  response_url: String,
  response_headers: fetch::Headers,
  server_addr: Option<RemoteAddr>,
  redirected: bool,
  unfollowed_redirect: bool,
  response_type: ResponseType,
  stream: fetch::ByteStream,
}

impl std::fmt::Debug for HttpStreamResponse {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("HttpStreamResponse")
      .field("status_code", &self.status_code)
      .field("response_url", &self.response_url)
      .field("redirected", &self.redirected)
      .finish_non_exhaustive()
  }
}

impl HttpStreamResponse {
  fn from_response(response: fetch::Response) -> Self {
    let fetch::Response {
      status,
      status_text,
      url,
      headers,
      body,
      redirected,
      unfollowed_redirect,
      server_addr,
      type_,
      ..
    } = response;
    Self {
      status_code: status,
      status_text,
      response_url: url,
      response_headers: headers,
      server_addr,
      redirected,
      unfollowed_redirect,
      response_type: type_,
      stream: body.into_stream(),
    }
  }

  #[must_use]
  pub fn status(&self) -> u16 {
    self.status_code
  }

  #[must_use]
  pub fn status_text(&self) -> &str {
    &self.status_text
  }

  #[must_use]
  pub fn url(&self) -> &str {
    &self.response_url
  }

  #[must_use]
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status_code)
  }

  /// Response headers verbatim: duplicates and casing preserved.
  #[must_use]
  pub fn headers(&self) -> &[(String, String)] {
    self.response_headers.entries()
  }

  /// Flattened header object: lowercased names, combined values.
  #[must_use]
  pub fn headers_object(&self) -> Vec<(String, String)> {
    self.response_headers.to_object()
  }

  /// Combined value of a header (case-insensitive), duplicates joined
  /// with `, ` — `\n` for `set-cookie`.
  #[must_use]
  pub fn header(&self, name: &str) -> Option<String> {
    self.response_headers.get(name)
  }

  /// Resolved peer address, or `None` when the transport didn't surface
  /// one (so `apiResponse.serverAddr()` works through the streamed path).
  #[must_use]
  pub fn server_addr(&self) -> Option<&RemoteAddr> {
    self.server_addr.as_ref()
  }

  /// Whether at least one redirect hop was followed (`Response.redirected`).
  #[must_use]
  pub fn redirected(&self) -> bool {
    self.redirected
  }

  /// Whether this 3xx was returned unfollowed because `redirect: manual`.
  #[must_use]
  pub fn unfollowed_redirect(&self) -> bool {
    self.unfollowed_redirect
  }

  /// WHATWG `Response.type` (basic / opaqueredirect / …).
  #[must_use]
  pub fn response_type(&self) -> ResponseType {
    self.response_type
  }

  /// Next body chunk, or `None` at end of stream.
  ///
  /// # Errors
  ///
  /// Returns an error if reading the body fails (connection reset, etc).
  pub async fn chunk(&mut self) -> crate::error::Result<Option<bytes::Bytes>> {
    self.stream.next().await.transpose().map_err(Into::into)
  }
}

/// A general HTTP client: all methods, JSON/form/multipart bodies,
/// query params, custom headers, timeouts, and cookie persistence. The
/// one stack `fetch` and `request` share.
#[derive(Clone)]
pub struct HttpClient {
  pool: fetch::ClientPool,
  base_url: Option<String>,
  extra_headers: Vec<(String, String)>,
  default_timeout: Duration,
  default_ignore_https: bool,
  /// Browser-context bridge. `Some` for a client minted by
  /// `ContextRef::http_client()` (Playwright's `page.request` /
  /// `context.request`): cookies are read from and written back to the
  /// BROWSER on every hop, defaults come live from the context options.
  /// `None` = the standalone client with its own reqwest jar.
  bridge: Option<Arc<dyn ContextBridge>>,
  /// The context a bound client reads and writes cookies through.
  context: Option<crate::context::ContextRef>,
  credentials: CredentialSource,
}

/// Playwright's `rewriteCookies` (`server/network.ts`): a cookie given
/// by `url` becomes the domain, the directory of the path and the
/// `secure` that URL stands for, before any jar sees it. A browser asked
/// to resolve the URL itself picks a different path, or drops the cookie.
///
/// # Errors
///
/// Returns an error for a cookie with neither a `url` nor a domain and
/// path, with both, or with an `expires` that is not `-1` or positive.
pub(crate) fn rewrite_cookie(
  mut cookie: crate::backend::CookieData,
) -> crate::error::Result<crate::backend::CookieData> {
  let invalid = |message: String| crate::error::FerriError::invalid_argument("cookies", message);
  match cookie.url.take() {
    Some(url) => {
      // The conversion from `SetCookieParams` fills an absent path in,
      // so only a domain beside the url can be told apart.
      if !cookie.domain.is_empty() {
        return Err(invalid("Cookie should have either url or domain".into()));
      }
      if url == "about:blank" {
        return Err(invalid(format!("Blank page can not have cookie \"{}\"", cookie.name)));
      }
      if url.starts_with("data:") {
        return Err(invalid(format!(
          "Data URL page can not have cookie \"{}\"",
          cookie.name
        )));
      }
      let parsed = reqwest::Url::parse(&url).map_err(|e| invalid(format!("Invalid cookie url {url:?}: {e}")))?;
      cookie.domain = parsed.host_str().unwrap_or_default().to_string();
      let path = parsed.path();
      cookie.path = path[..=path.rfind('/').unwrap_or(0)].to_string();
      cookie.secure = parsed.scheme() == "https";
    },
    None if cookie.domain.is_empty() || cookie.path.is_empty() => {
      return Err(invalid("Cookie should have a url or a domain/path pair".into()));
    },
    None => {},
  }
  if cookie
    .expires
    .is_some_and(|expires| expires < 0.0 && (expires + 1.0).abs() > f64::EPSILON)
  {
    return Err(invalid(
      "Cookie should have a valid expires, only -1 or a positive number for the unix timestamp in seconds is allowed"
        .into(),
    ));
  }
  Ok(cookie)
}

/// A cookie as Playwright's request context stores it
/// (`GlobalAPIRequestContext.addCookies`): rewritten, a session cookie
/// unless it says when it expires, `Lax` unless it says otherwise.
fn stored_cookie(cookie: crate::backend::CookieData) -> crate::error::Result<crate::backend::CookieData> {
  let mut cookie = rewrite_cookie(cookie)?;
  cookie.expires.get_or_insert(-1.0);
  cookie.same_site.get_or_insert(crate::backend::SameSite::Lax);
  Ok(cookie)
}

/// Playwright's `filterCookies`: the cookies a request to any of `urls`
/// would carry, or all of them without a URL.
///
/// # Errors
///
/// Returns an error for a URL that does not parse.
pub fn cookies_for_urls(
  cookies: Vec<crate::backend::CookieData>,
  urls: &[String],
) -> crate::error::Result<Vec<crate::backend::CookieData>> {
  if urls.is_empty() {
    return Ok(cookies);
  }
  let urls = urls
    .iter()
    .map(|url| {
      reqwest::Url::parse(url)
        .map_err(|e| crate::error::FerriError::invalid_argument("urls", format!("Invalid URL {url:?}: {e}")))
    })
    .collect::<crate::error::Result<Vec<_>>>()?;
  Ok(
    cookies
      .into_iter()
      .filter(|cookie| urls.iter().any(|url| fetch::cookie::cookie_matches_url(cookie, url)))
      .collect(),
  )
}

impl HttpClient {
  /// Create a new standalone HTTP client (its own reqwest cookie jar).
  #[must_use]
  pub fn new(options: HttpClientOptions) -> Self {
    Self {
      pool: fetch::ClientPool::standalone(options.ignore_https_errors),
      base_url: options.base_url,
      extra_headers: options.extra_http_headers,
      default_timeout: options.timeout.unwrap_or(Duration::from_secs(30)),
      default_ignore_https: options.ignore_https_errors,
      bridge: None,
      context: None,
      credentials: CredentialSource::Fixed(options.http_credentials),
    }
  }

  /// Create a client bound to a browser context (Playwright's
  /// `page.request` / `context.request`). All cookie state lives in the
  /// browser via `bridge`; defaults (`baseURL`, extra headers, UA,
  /// `ignoreHTTPSErrors`) are read live from the context on every
  /// request.
  #[must_use]
  pub fn context_bound(bridge: Arc<dyn ContextBridge>) -> Self {
    Self {
      pool: fetch::ClientPool::bridged(),
      base_url: None,
      extra_headers: Vec::new(),
      default_timeout: Duration::from_secs(30),
      default_ignore_https: false,
      bridge: Some(bridge),
      context: None,
      credentials: CredentialSource::Fixed(Vec::new()),
    }
  }

  /// The context whose cookies `cookies()` / `add_cookies()` /
  /// `clear_cookies()` read and write, for a client bound to one.
  #[must_use]
  pub fn with_context(mut self, context: crate::context::ContextRef) -> Self {
    self.context = Some(context);
    self
  }

  /// Playwright: `apiRequestContext.cookies(urls?)`. The cookies a
  /// request to any of `urls` would carry, or every cookie without one:
  /// the browser context's for a bound client, this client's own jar
  /// otherwise.
  ///
  /// # Errors
  ///
  /// Returns an error for a URL that does not parse, or if the context's
  /// cookies cannot be read.
  pub async fn cookies(&self, urls: &[String]) -> crate::error::Result<Vec<crate::backend::CookieData>> {
    let all = match (&self.context, self.pool.jar()) {
      (Some(context), _) => match context.cookies().await {
        Ok(cookies) => cookies,
        // A context has no jar in the browser before its first page.
        Err(crate::error::FerriError::InvalidArgument { .. }) => Vec::new(),
        Err(e) => return Err(e),
      },
      (None, Some(jar)) => jar.all(),
      (None, None) => Vec::new(),
    };
    cookies_for_urls(all, urls)
  }

  /// Playwright: `apiRequestContext.addCookies(cookies)`.
  ///
  /// # Errors
  ///
  /// Returns an error for a cookie with neither a `url` nor a
  /// `domain`/`path` pair, or if the context refuses it.
  pub async fn add_cookies(&self, cookies: Vec<crate::backend::CookieData>) -> crate::error::Result<()> {
    match (&self.context, self.pool.jar()) {
      (Some(context), _) => context.add_cookies(cookies).await,
      (None, Some(jar)) => {
        let cookies = cookies
          .into_iter()
          .map(stored_cookie)
          .collect::<crate::error::Result<Vec<_>>>()?;
        jar.add(cookies);
        Ok(())
      },
      (None, None) => Ok(()),
    }
  }

  /// Playwright: `apiRequestContext.clearCookies(options?)`. Without a
  /// filter every cookie goes; with one, those matching all of it.
  ///
  /// # Errors
  ///
  /// Returns an error if a filter's regex does not compile, or if the
  /// context's cookies cannot be cleared.
  pub async fn clear_cookies(&self, options: &crate::backend::ClearCookieOptions) -> crate::error::Result<()> {
    match (&self.context, self.pool.jar()) {
      (Some(context), _) => context.clear_cookies_filtered(options).await,
      (None, Some(jar)) => {
        let filter = options.compile()?;
        jar.remove(|cookie| filter.matches(cookie));
        Ok(())
      },
      (None, None) => Ok(()),
    }
  }

  /// Answer challenges with whatever `credentials` returns at the time.
  #[must_use]
  pub fn with_live_http_credentials(mut self, credentials: LiveHttpCredentials) -> Self {
    self.credentials = CredentialSource::Live(credentials);
    self
  }

  async fn http_credentials(&self) -> Vec<HttpCredentials> {
    match &self.credentials {
      CredentialSource::Fixed(list) => list.clone(),
      CredentialSource::Live(read) => read().await,
    }
  }

  /// Send `request`, letting the engine answer a Basic challenge with
  /// the credentials matching the URL that raised it, as Playwright's
  /// `_sendRequest` does (`server/fetch.ts`): a 401 to a hop that carried
  /// no `authorization` is sent again with one, a hop reached through a
  /// redirect to another origin included.
  async fn send_answering_challenge(&self, mut request: fetch::Request) -> crate::error::Result<fetch::Response> {
    let credentials = self.http_credentials().await;
    if !credentials.is_empty() {
      request.challenge = Some(fetch::ChallengeAnswer::new(move |url| {
        crate::options::matching_http_credentials(&credentials, url.as_str()).map(basic_authorization)
      }));
    }
    Ok(fetch::send(&self.pool, self.bridge.as_ref(), request).await?)
  }

  /// Effective per-request defaults. Bridged: read live from the browser
  /// context (so `setExtraHTTPHeaders` etc. take effect); plus a
  /// `user-agent` + `accept: */*` base like Playwright's `_sendRequest`.
  /// Standalone: from the client options; reqwest supplies UA/accept.
  ///
  /// Shared by the Playwright option-bag path and the WHATWG `fetch`
  /// path so both resolve the base URL, context headers and TLS posture
  /// identically — only body/content-type handling differs between them.
  async fn resolved_defaults(&self) -> crate::error::Result<ResolvedDefaults> {
    let (base_url, mut extra_headers, user_agent, ignore_https_errors, ctx_style) = if let Some(bridge) = &self.bridge {
      let d = bridge.defaults().await?;
      (
        d.base_url,
        d.extra_http_headers,
        d.user_agent,
        d.ignore_https_errors,
        true,
      )
    } else {
      (
        self.base_url.clone(),
        Vec::new(),
        None,
        self.default_ignore_https,
        false,
      )
    };
    extra_headers.extend(self.extra_headers.iter().cloned());
    Ok(ResolvedDefaults {
      base_url,
      extra_headers,
      user_agent,
      ignore_https_errors,
      ctx_style,
    })
  }

  /// The default + context headers every request starts from, before the
  /// caller's own.
  fn base_headers(defaults: &ResolvedDefaults) -> fetch::Headers {
    let mut headers = fetch::Headers::new();
    if defaults.ctx_style {
      if let Some(ua) = &defaults.user_agent {
        headers.set("user-agent", ua.clone());
      }
      headers.set("accept", "*/*");
    }
    for (name, value) in &defaults.extra_headers {
      headers.set(name, value.clone());
    }
    headers
  }

  /// Send a request assembled by the WHATWG `fetch` layer.
  ///
  /// The caller has already run the spec's "extract a body" step, so the
  /// body and its `content-type` arrive decided. That is the whole point
  /// of this entry point: `Self::build_request` applies Playwright's
  /// `data` defaults (notably `content-type: application/octet-stream`),
  /// which are right for `request.post(url, { data })` and wrong for
  /// `fetch(url, { body: someArrayBuffer })` — where the spec sends no
  /// `content-type` at all.
  ///
  /// # Errors
  ///
  /// Returns an error if the URL cannot be resolved, the method is
  /// invalid, or the request fails.
  pub async fn fetch_whatwg(&self, request: WhatwgRequest) -> crate::error::Result<HttpStreamResponse> {
    Ok(HttpStreamResponse::from_response(self.send_whatwg(request).await?))
  }

  /// [`Self::fetch_whatwg`] answering the engine's own
  /// [`fetch::Response`], for a caller that streams the body itself
  /// (the runtime's `fetch` global over this client).
  ///
  /// # Errors
  ///
  /// The engine's typed failure, so a permission refusal stays one.
  pub async fn send_whatwg(&self, request: WhatwgRequest) -> Result<fetch::Response, fetch::FetchError> {
    let defaults = self
      .resolved_defaults()
      .await
      .map_err(|e| fetch::FetchError::Bridge(e.to_string()))?;
    let url = resolve_url(defaults.base_url.as_deref(), &request.url)
      .map_err(|e| fetch::FetchError::InvalidUrl(e.to_string()))?;
    let method: reqwest::Method = request
      .method
      .parse()
      .map_err(|_| fetch::FetchError::InvalidUrl(format!("invalid HTTP method: {}", request.method)))?;

    let mut headers = Self::base_headers(&defaults);
    for (name, value) in request.headers {
      headers.set(&name, value);
    }

    fetch::send(
      &self.pool,
      self.bridge.as_ref(),
      fetch::Request {
        method,
        url,
        headers,
        body: request.body,
        redirect: request.redirect,
        credentials: request.credentials,
        max_redirects: None,
        max_retries: 0,
        timeout: request.timeout.unwrap_or(self.default_timeout),
        ignore_https_errors: defaults.ignore_https_errors,
        net_guard: request.net_guard,
        challenge: None,
      },
    )
    .await
  }

  /// Lower the Playwright option bag into a [`fetch::Request`], resolving
  /// the URL/defaults live (from the context bridge when bound, else from
  /// the client options) and materializing the body + content-type.
  async fn build_request(&self, url: &str, opts: &RequestOptions) -> crate::error::Result<fetch::Request> {
    let defaults = self.resolved_defaults().await?;
    let ignore_default = defaults.ignore_https_errors;

    let mut request_url = resolve_url(defaults.base_url.as_deref(), url)?;
    if let Some(params) = &opts.params {
      let mut qp = request_url.query_pairs_mut();
      for (k, v) in params {
        qp.append_pair(k, v);
      }
    }

    let method_str = opts.method.as_deref().unwrap_or("GET");
    let method: reqwest::Method = method_str
      .parse()
      .map_err(|_| crate::error::FerriError::Backend(format!("invalid HTTP method: {method_str}")))?;

    // Default headers, then context extras, then per-request headers —
    // later writers replace earlier ones (fetch.ts:178).
    let mut headers = Self::base_headers(&defaults);
    if let Some(request_headers) = &opts.headers {
      for (k, v) in request_headers {
        headers.set(k, v.clone());
      }
    }

    // `send: 'always'` puts the header on before any challenge, over one
    // the caller set (fetch.ts sets it after the request's own headers).
    let credentials = self.http_credentials().await;
    if let Some(entry) = crate::options::matching_http_credentials(&credentials, request_url.as_str())
      && entry.send == Some(HttpCredentialsSend::Always)
    {
      headers.set("authorization", basic_authorization(entry));
    }

    // Request body (mutually exclusive: multipart, json, form, raw data).
    let body = if let Some(fields) = &opts.multipart {
      let (bytes, content_type) = fetch::serialize_multipart(fields, &fetch::multipart_boundary());
      headers.set("content-type", content_type);
      fetch::Body::from_bytes(bytes)
    } else if let Some(json) = &opts.json_data {
      headers.set_if_absent("content-type", "application/json");
      fetch::Body::from_bytes(serde_json::to_vec(json)?)
    } else if let Some(form) = &opts.form {
      headers.set_if_absent("content-type", "application/x-www-form-urlencoded");
      let encoded = serde_urlencoded::to_string(form)
        .map_err(|e| crate::error::FerriError::Backend(format!("serialize form data: {e}")))?;
      fetch::Body::from_bytes(encoded.into_bytes())
    } else if let Some(data) = &opts.data {
      headers.set_if_absent("content-type", "application/octet-stream");
      fetch::Body::from_bytes(data.clone())
    } else {
      fetch::Body::empty()
    };

    Ok(fetch::Request {
      method,
      url: request_url,
      headers,
      body,
      redirect: opts.redirect,
      credentials: opts.credentials.unwrap_or_default(),
      max_redirects: opts.max_redirects,
      max_retries: opts.max_retries.unwrap_or(0),
      timeout: opts.timeout.unwrap_or(self.default_timeout),
      ignore_https_errors: opts.ignore_https_errors.unwrap_or(ignore_default),
      net_guard: opts.net_guard.clone(),
      challenge: None,
    })
  }

  /// Send a GET request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn get(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self.fetch(url, Some(with_method(options, "GET"))).await
  }

  /// Send a POST request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn post(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self.fetch(url, Some(with_method(options, "POST"))).await
  }

  /// Send a PUT request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn put(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self.fetch(url, Some(with_method(options, "PUT"))).await
  }

  /// Send a DELETE request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn delete(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self.fetch(url, Some(with_method(options, "DELETE"))).await
  }

  /// Send a PATCH request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn patch(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self.fetch(url, Some(with_method(options, "PATCH"))).await
  }

  /// Send a HEAD request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn head(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self.fetch(url, Some(with_method(options, "HEAD"))).await
  }

  /// Send an HTTP request (generic method — all verbs delegate here).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or `fail_on_status_code` is set and the response is 4xx/5xx.
  pub async fn fetch(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    let opts = options.unwrap_or_default();
    let request = self.build_request(url, &opts).await?;
    let response = self.send_answering_challenge(request).await?;
    let http = HttpResponse::buffer(response).await?;
    if opts.fail_on_status_code.unwrap_or(false) && !http.ok() {
      return Err(crate::error::FerriError::Backend(format!(
        "{} {} failed: {} {}",
        opts.method.as_deref().unwrap_or("GET"),
        http.url(),
        http.status(),
        http.status_text()
      )));
    }
    Ok(http)
  }

  /// Like [`Self::fetch`] but the body is NOT buffered: returns the
  /// status/headers plus a handle whose [`HttpStreamResponse::chunk`]
  /// yields bytes as they arrive (backs a WHATWG `Response.body`).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails, or `fail_on_status_code` is
  /// set and the response is 4xx/5xx (checked before any body is read).
  pub async fn fetch_stream(
    &self,
    url: &str,
    options: Option<RequestOptions>,
  ) -> crate::error::Result<HttpStreamResponse> {
    let opts = options.unwrap_or_default();
    let request = self.build_request(url, &opts).await?;
    let response = self.send_answering_challenge(request).await?;
    if opts.fail_on_status_code.unwrap_or(false) && !response.ok() {
      return Err(crate::error::FerriError::Backend(format!(
        "{} {} failed: {} {}",
        opts.method.as_deref().unwrap_or("GET"),
        response.url,
        response.status,
        response.status_text
      )));
    }
    Ok(HttpStreamResponse::from_response(response))
  }

  /// Dispose the request context (Playwright compat).
  pub fn dispose(self) {
    drop(self);
  }
}

impl RequestOptions {
  /// # Errors
  /// Rejects modes outside `follow`, `manual`, and `error` before any request is sent.
  pub fn parse_redirect(value: &str) -> Result<RedirectMode, String> {
    match value {
      "follow" => Ok(RedirectMode::Follow),
      "manual" => Ok(RedirectMode::Manual),
      "error" => Ok(RedirectMode::Error),
      _ => Err(format!("invalid redirect mode: {value}")),
    }
  }

  /// Lower a Playwright `params` / `form` option map, whose values the
  /// types admit as `string | number | boolean`, into ordered pairs.
  ///
  /// Both bindings call this so the two of them cannot disagree about
  /// what a `params` bag means or word the rejection differently.
  ///
  /// # Errors
  ///
  /// Returns a message naming the offending key when a value is not a
  /// scalar. `field` is the option name used in that message.
  pub fn scalar_map_to_pairs<I>(field: &str, map: I) -> Result<Vec<(String, String)>, String>
  where
    I: IntoIterator<Item = (String, serde_json::Value)>,
  {
    map
      .into_iter()
      .map(|(key, value)| {
        let text = match value {
          serde_json::Value::String(s) => s,
          serde_json::Value::Number(n) => n.to_string(),
          serde_json::Value::Bool(b) => b.to_string(),
          other => {
            return Err(format!(
              "{field}[{key:?}] must be a string, number, or boolean (got {other})"
            ));
          },
        };
        Ok((key, text))
      })
      .collect()
  }

  /// Playwright's `data` routing (`client/fetch.ts` `serializePostData`)
  /// as the `(data, json_data)` pair [`RequestOptions`] carries: a
  /// string is a raw body, a byte array is a raw body, and any other
  /// serializable value is sent as JSON. An explicit `json` always wins.
  ///
  /// Shared so the NAPI and `QuickJS` bindings cannot route the same
  /// `data` value to different bodies — which they did, NAPI JSON-ifying
  /// the byte arrays `QuickJS` sent raw.
  #[must_use]
  pub fn split_data(
    data: Option<serde_json::Value>,
    json: Option<serde_json::Value>,
  ) -> (Option<Vec<u8>>, Option<serde_json::Value>) {
    match (data, json) {
      (_, Some(json)) => (None, Some(json)),
      (Some(serde_json::Value::String(s)), None) => (Some(s.into_bytes()), None),
      (Some(value), None) if value.is_array() => match serde_json::from_value::<Vec<u8>>(value.clone()) {
        Ok(bytes) => (Some(bytes), None),
        Err(_) => (None, Some(value)),
      },
      (Some(value), None) => (None, Some(value)),
      (None, None) => (None, None),
    }
  }
}

/// Return `options` with its method set to `method` (verb convenience
/// helpers all funnel through [`HttpClient::fetch`]).
fn with_method(options: Option<RequestOptions>, method: &str) -> RequestOptions {
  RequestOptions {
    method: Some(method.to_string()),
    ..options.unwrap_or_default()
  }
}

/// Resolve `url` against `base_url` with full URL-join semantics
/// (`new URL(url, baseURL)`): an absolute URL is used as-is; a relative
/// one joins onto the base.
fn resolve_url(base_url: Option<&str>, url: &str) -> crate::error::Result<reqwest::Url> {
  match reqwest::Url::parse(url) {
    Ok(u) => Ok(u),
    Err(_) => match base_url {
      Some(base) => reqwest::Url::parse(base)
        .and_then(|b| b.join(url))
        .map_err(|e| crate::error::FerriError::Backend(format!("invalid URL \"{url}\" against baseURL {base:?}: {e}"))),
      None => Err(crate::error::FerriError::Backend(format!(
        "invalid URL \"{url}\": no baseURL to resolve against"
      ))),
    },
  }
}

#[cfg(test)]
mod response_tests {
  use super::*;

  fn cookie(url: Option<&str>, domain: &str, path: &str) -> crate::backend::CookieData {
    crate::backend::CookieData {
      name: "a".into(),
      value: "1".into(),
      domain: domain.into(),
      path: path.into(),
      secure: false,
      http_only: false,
      expires: None,
      same_site: None,
      url: url.map(str::to_string),
    }
  }

  #[test]
  fn a_cookie_url_becomes_its_host_directory_and_scheme() {
    let rewritten = |url: &str| {
      let cookie = rewrite_cookie(cookie(Some(url), "", "")).unwrap();
      (cookie.domain, cookie.path, cookie.secure, cookie.url)
    };
    assert_eq!(
      rewritten("https://example.com/app/page.html?q=1"),
      ("example.com".into(), "/app/".into(), true, None)
    );
    assert_eq!(
      rewritten("http://example.com"),
      ("example.com".into(), "/".into(), false, None)
    );
    assert_eq!(
      rewritten("http://127.0.0.1:8080/a/b/"),
      ("127.0.0.1".into(), "/a/b/".into(), false, None)
    );

    let refused = |cookie| rewrite_cookie(cookie).unwrap_err().to_string();
    assert!(refused(cookie(None, "", "/")).contains("a url or a domain/path pair"));
    assert!(refused(cookie(None, "example.com", "")).contains("a url or a domain/path pair"));
    assert!(refused(cookie(Some("http://example.com/"), "example.com", "")).contains("either url or domain"));
    assert!(refused(cookie(Some("about:blank"), "", "")).contains("Blank page can not have cookie \"a\""));
    assert!(refused(cookie(Some("data:text/plain,x"), "", "")).contains("Data URL page can not have cookie"));
    let mut expired = cookie(None, "example.com", "/");
    expired.expires = Some(-5.0);
    assert!(refused(expired).contains("valid expires"));
    let mut session = cookie(None, "example.com", "/");
    session.expires = Some(-1.0);
    assert!(rewrite_cookie(session).is_ok());

    let stored = stored_cookie(cookie(None, "example.com", "/")).unwrap();
    assert_eq!(
      (stored.expires, stored.same_site),
      (Some(-1.0), Some(crate::backend::SameSite::Lax))
    );
  }

  #[test]
  fn timing_is_milliseconds_from_the_start_with_absent_phases_at_minus_one() {
    let start = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let new_connection = resource_timing(
      &fetch::Timing {
        start_time: start,
        dns_end: Some(Duration::from_millis(3)),
        connect_end: Some(Duration::from_millis(10)),
        response_start: Duration::from_millis(25),
      },
      start + Duration::from_millis(40),
    );
    assert_eq!(
      (
        new_connection.start_time,
        new_connection.domain_lookup_start,
        new_connection.domain_lookup_end,
        new_connection.connect_start,
        new_connection.secure_connection_start,
        new_connection.connect_end,
        new_connection.request_start,
        new_connection.response_start,
        new_connection.response_end,
      ),
      (1_700_000_000_000.0, 0.0, 3.0, 3.0, -1.0, 10.0, 10.0, 25.0, 40.0)
    );

    let reused = resource_timing(
      &fetch::Timing {
        start_time: start,
        dns_end: None,
        connect_end: None,
        response_start: Duration::from_millis(5),
      },
      // A clock that stepped back never reports an end before the head.
      start,
    );
    assert_eq!(
      (
        reused.domain_lookup_start,
        reused.domain_lookup_end,
        reused.connect_start,
        reused.connect_end,
        reused.request_start,
        reused.response_start,
        reused.response_end,
      ),
      (-1.0, -1.0, -1.0, -1.0, 0.0, 5.0, 5.0)
    );

    // An address literal connects without a lookup.
    let literal = resource_timing(
      &fetch::Timing {
        start_time: start,
        dns_end: None,
        connect_end: Some(Duration::from_millis(2)),
        response_start: Duration::from_millis(4),
      },
      start + Duration::from_millis(4),
    );
    assert_eq!(
      (literal.domain_lookup_end, literal.connect_start, literal.connect_end),
      (-1.0, 0.0, 2.0)
    );
  }

  fn response(headers: Vec<(&str, &str)>, body: &[u8]) -> HttpResponse {
    HttpResponse {
      status_code: 200,
      status_text: "OK".into(),
      response_url: "http://example.test/".into(),
      response_headers: fetch::Headers::from_pairs(
        headers
          .into_iter()
          .map(|(k, v)| (k.to_string(), v.to_string()))
          .collect(),
      ),
      body_bytes: bytes::Bytes::copy_from_slice(body),
      server_addr: None,
      redirected: false,
      unfollowed_redirect: false,
      response_type: ResponseType::Basic,
      timing: crate::network::RequestTiming::default(),
      peer_certificate: None,
      disposed: false,
    }
  }

  #[test]
  fn header_combines_duplicates_like_playwright_rawheaders() {
    let r = response(
      vec![
        ("X-Dup", "one"),
        ("Set-Cookie", "a=1"),
        ("x-dup", "two"),
        ("set-cookie", "b=2"),
      ],
      b"",
    );
    assert_eq!(r.header("x-dup").as_deref(), Some("one, two"));
    assert_eq!(r.header("X-DUP").as_deref(), Some("one, two"), "case-insensitive");
    assert_eq!(r.header("set-cookie").as_deref(), Some("a=1\nb=2"));
    assert_eq!(r.header("absent"), None);
  }

  #[test]
  fn headers_array_stays_verbatim_while_headers_object_flattens() {
    let r = response(
      vec![("Content-Type", "text/plain"), ("X-Dup", "one"), ("x-dup", "two")],
      b"",
    );
    assert_eq!(
      r.headers(),
      [
        ("Content-Type".to_string(), "text/plain".to_string()),
        ("X-Dup".to_string(), "one".to_string()),
        ("x-dup".to_string(), "two".to_string()),
      ]
    );
    assert_eq!(
      r.headers_object(),
      vec![
        ("content-type".to_string(), "text/plain".to_string()),
        ("x-dup".to_string(), "one, two".to_string()),
      ]
    );
  }

  #[test]
  fn dispose_releases_the_body_but_keeps_the_metadata() {
    let mut r = response(vec![("content-type", "application/json")], br#"{"a":1}"#);
    assert_eq!(r.text().expect("text before dispose"), r#"{"a":1}"#);

    r.dispose();

    assert!(matches!(r.body(), Err(crate::error::FerriError::Disposed("Response"))));
    assert!(r.text().is_err());
    assert!(r.json_value().is_err());
    assert_eq!(
      r.body().unwrap_err().to_string(),
      "Response has been disposed",
      "message mirrors Playwright's"
    );
    // Metadata survives disposal, as it does in Playwright.
    assert_eq!(r.status(), 200);
    assert_eq!(r.url(), "http://example.test/");
    assert_eq!(r.header("content-type").as_deref(), Some("application/json"));
  }
}

#[cfg(test)]
mod context_bound_tests {
  use super::*;
  use std::io::{Read as _, Write as _};
  use std::sync::Mutex as StdMutex;

  /// In-memory stand-in for the browser context: a cookie store with
  /// browser-like (name, domain, path) replacement semantics.
  struct MockBridge {
    defaults: ContextDefaults,
    cookies: StdMutex<Vec<crate::backend::CookieData>>,
  }

  impl MockBridge {
    fn new() -> Arc<Self> {
      Arc::new(Self {
        defaults: ContextDefaults::default(),
        cookies: StdMutex::new(Vec::new()),
      })
    }

    fn seed(self: &Arc<Self>, cookie: crate::backend::CookieData) {
      self.cookies.lock().unwrap().push(cookie);
    }

    fn cookie_value(&self, name: &str) -> Option<String> {
      self
        .cookies
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.value.clone())
    }
  }

  impl ContextBridge for MockBridge {
    fn defaults(&self) -> BridgeFuture<'_, ContextDefaults> {
      let d = self.defaults.clone();
      Box::pin(async move { Ok(d) })
    }

    fn cookies(&self) -> BridgeFuture<'_, Vec<crate::backend::CookieData>> {
      let c = self.cookies.lock().unwrap().clone();
      Box::pin(async move { Ok(c) })
    }

    fn add_cookies(&self, new: Vec<crate::backend::CookieData>) -> BridgeFuture<'_, ()> {
      let mut store = self.cookies.lock().unwrap();
      for cookie in new {
        store.retain(|e| !(e.name == cookie.name && e.domain == cookie.domain && e.path == cookie.path));
        store.push(cookie);
      }
      Box::pin(async move { Ok(()) })
    }
  }

  fn cookie(name: &str, value: &str, domain: &str) -> crate::backend::CookieData {
    crate::backend::CookieData {
      name: name.into(),
      value: value.into(),
      domain: domain.into(),
      path: "/".into(),
      secure: false,
      http_only: false,
      expires: None,
      same_site: None,
      url: None,
    }
  }

  /// One scripted route: status, extra headers, body.
  type RouteResponse = (u16, Vec<(String, String)>, &'static str);

  /// Thread-per-connection test server (a serial accept loop starves
  /// behind browser-style idle preconnections; see CLAUDE.md). Routes:
  /// scripted per test via a match on the request path. Records every
  /// "METHOD path COOKIE:<header> CT:<header> CL:<header>" line.
  fn spawn_server(routes: fn(&str) -> RouteResponse) -> (String, Arc<StdMutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let log_srv = Arc::clone(&log);
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let log = Arc::clone(&log_srv);
        std::thread::spawn(move || {
          let mut buf = Vec::new();
          let mut byte = [0u8; 1];
          // Read until end of headers.
          while !buf.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            buf.push(byte[0]);
          }
          let text = String::from_utf8_lossy(&buf).to_string();
          let mut lines = text.lines();
          let request_line = lines.next().unwrap_or("").to_string();
          let mut method_path = request_line.split(' ');
          let method = method_path.next().unwrap_or("").to_string();
          let path = method_path.next().unwrap_or("").to_string();
          let mut cookie_header = String::new();
          let mut content_type = String::new();
          let mut content_length = 0usize;
          let mut user_agent = String::new();
          let mut x_ctx = String::new();
          for line in lines {
            let Some((k, v)) = line.split_once(':') else { continue };
            match k.to_ascii_lowercase().as_str() {
              "cookie" => cookie_header = v.trim().to_string(),
              "content-type" => content_type = v.trim().to_string(),
              "content-length" => content_length = v.trim().parse().unwrap_or(0),
              "user-agent" => user_agent = v.trim().to_string(),
              "x-ctx" => x_ctx = v.trim().to_string(),
              _ => {},
            }
          }
          let mut body = vec![0u8; content_length];
          if content_length > 0 {
            let _ = stream.read_exact(&mut body);
          }
          log.lock().unwrap().push(format!(
            "{method} {path} COOKIE:{cookie_header} CT:{content_type} CL:{content_length} UA:{user_agent} XCTX:{x_ctx}"
          ));
          let (status, headers, body) = routes(&path);
          let mut resp = format!(
            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
          );
          for (k, v) in headers {
            use std::fmt::Write as _;
            let _ = write!(resp, "{k}: {v}\r\n");
          }
          resp.push_str("\r\n");
          resp.push_str(body);
          let _ = stream.write_all(resp.as_bytes());
          let _ = stream.shutdown(std::net::Shutdown::Both);
        });
      }
    });
    (format!("http://{addr}"), log)
  }

  /// A server that wants `user:pass` and challenges with `scheme`. The log
  /// holds the `authorization` header of each request, empty when absent.
  fn spawn_auth_server(scheme: &'static str) -> (String, Arc<StdMutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
    let log_srv = Arc::clone(&log);
    std::thread::spawn(move || {
      for stream in listener.incoming() {
        let Ok(mut stream) = stream else { break };
        let log = Arc::clone(&log_srv);
        std::thread::spawn(move || {
          let mut buf = Vec::new();
          let mut byte = [0u8; 1];
          while !buf.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            buf.push(byte[0]);
          }
          let text = String::from_utf8_lossy(&buf).to_string();
          let authorization = text
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.trim().to_string())
            .unwrap_or_default();
          log.lock().unwrap().push(authorization.clone());
          // base64("user:pass")
          let resp = if authorization == "Basic dXNlcjpwYXNz" {
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nAUTHED".to_string()
          } else {
            format!(
              "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: {scheme} realm=\"x\"\r\nContent-Length: 6\r\nConnection: close\r\n\r\nNOAUTH"
            )
          };
          let _ = stream.write_all(resp.as_bytes());
          let _ = stream.shutdown(std::net::Shutdown::Both);
        });
      }
    });
    (format!("http://{addr}"), log)
  }

  fn credentials(password: &str, origin: Option<&str>, send: Option<HttpCredentialsSend>) -> HttpCredentials {
    HttpCredentials {
      username: "user".into(),
      password: password.into(),
      origin: origin.map(str::to_string),
      send,
    }
  }

  fn client_with(http_credentials: Vec<HttpCredentials>) -> HttpClient {
    HttpClient::new(HttpClientOptions {
      http_credentials,
      ..HttpClientOptions::default()
    })
  }

  #[tokio::test]
  async fn a_basic_challenge_is_answered_once_with_the_matching_credentials() {
    let (base, log) = spawn_auth_server("Basic");
    let client = client_with(vec![
      credentials("elsewhere", Some("http://elsewhere.example.com"), None),
      credentials("pass", Some(&base), None),
    ]);
    let resp = client.get(&format!("{base}/"), None).await.unwrap();
    assert_eq!((resp.status(), resp.text().unwrap().as_str()), (200, "AUTHED"));
    assert_eq!(*log.lock().unwrap(), ["", "Basic dXNlcjpwYXNz"]);
  }

  #[tokio::test]
  async fn credentials_sent_always_go_out_before_any_challenge() {
    let (base, log) = spawn_auth_server("Basic");
    let client = client_with(vec![credentials("pass", None, Some(HttpCredentialsSend::Always))]);
    let resp = client.get(&format!("{base}/"), None).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(*log.lock().unwrap(), ["Basic dXNlcjpwYXNz"]);
  }

  #[tokio::test]
  async fn refused_credentials_are_not_tried_again() {
    let (base, log) = spawn_auth_server("Basic");
    let client = client_with(vec![credentials("wrong", None, None)]);
    let resp = client.get(&format!("{base}/"), None).await.unwrap();
    assert_eq!(resp.status(), 401);
    // base64("user:wrong")
    assert_eq!(*log.lock().unwrap(), ["", "Basic dXNlcjp3cm9uZw=="]);
  }

  #[tokio::test]
  async fn a_challenge_nothing_matches_is_returned_as_it_came() {
    for (scheme, list, headers) in [
      (
        "Basic",
        vec![credentials("pass", Some("http://elsewhere.example.com"), None)],
        None,
      ),
      ("Basic", Vec::new(), None),
      ("Digest", vec![credentials("pass", None, None)], None),
      // The caller's own header is the answer it chose.
      (
        "Basic",
        vec![credentials("pass", None, None)],
        Some(vec![("Authorization".to_string(), "Bearer mine".to_string())]),
      ),
    ] {
      let (base, log) = spawn_auth_server(scheme);
      let sent = headers.as_ref().map_or("", |headers| headers[0].1.as_str()).to_string();
      let options = RequestOptions {
        headers,
        ..RequestOptions::default()
      };
      let resp = client_with(list).get(&format!("{base}/"), Some(options)).await.unwrap();
      assert_eq!(resp.status(), 401, "{scheme}");
      assert_eq!(*log.lock().unwrap(), [sent], "{scheme}");
    }
  }

  #[tokio::test]
  async fn a_context_bound_client_reads_its_credentials_when_challenged() {
    let (base, log) = spawn_auth_server("Basic");
    let current: Arc<StdMutex<Vec<HttpCredentials>>> = Arc::default();
    let read = Arc::clone(&current);
    let client = HttpClient::context_bound(MockBridge::new()).with_live_http_credentials(Arc::new(move || {
      let list = read.lock().unwrap().clone();
      Box::pin(async move { list })
    }));
    assert_eq!(client.get(&format!("{base}/"), None).await.unwrap().status(), 401);
    *current.lock().unwrap() = vec![credentials("pass", None, None)];
    assert_eq!(client.get(&format!("{base}/"), None).await.unwrap().status(), 200);
    assert_eq!(*log.lock().unwrap(), ["", "", "Basic dXNlcjpwYXNz"]);
  }

  #[tokio::test]
  async fn bridge_cookie_header_injected() {
    let (base, log) = spawn_server(|_| (200, vec![], "ok"));
    let bridge = MockBridge::new();
    bridge.seed(cookie("sid", "abc", "127.0.0.1"));
    // A cookie for another domain must not leak in.
    bridge.seed(cookie("other", "x", "example.com"));
    let client = HttpClient::context_bound(bridge);
    let resp = client.get(&format!("{base}/echo"), None).await.unwrap();
    assert_eq!(resp.status(), 200);
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert!(log[0].contains("COOKIE:sid=abc "), "got {:?}", log[0]);
  }

  #[tokio::test]
  async fn set_cookie_written_back_to_bridge() {
    let (base, _log) = spawn_server(|path| {
      if path == "/set" {
        (200, vec![("Set-Cookie".into(), "sid=fresh; Path=/".into())], "ok")
      } else {
        (200, vec![], "ok")
      }
    });
    let bridge = MockBridge::new();
    let client = HttpClient::context_bound(Arc::clone(&bridge) as Arc<dyn ContextBridge>);
    client.get(&format!("{base}/set"), None).await.unwrap();
    assert_eq!(bridge.cookie_value("sid").as_deref(), Some("fresh"));
  }

  #[tokio::test]
  async fn redirect_hop_set_cookie_captured_and_reinjected() {
    let (base, log) = spawn_server(|path| {
      if path == "/hop" {
        (
          302,
          vec![
            ("Set-Cookie".into(), "hop=1; Path=/".into()),
            ("Location".into(), "/after".into()),
          ],
          "",
        )
      } else {
        (200, vec![], "landed")
      }
    });
    let bridge = MockBridge::new();
    let client = HttpClient::context_bound(Arc::clone(&bridge) as Arc<dyn ContextBridge>);
    let resp = client.get(&format!("{base}/hop"), None).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().unwrap(), "landed");
    // The redirect hop's Set-Cookie reached the bridge...
    assert_eq!(bridge.cookie_value("hop").as_deref(), Some("1"));
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2);
    // ...and was already sent on the SECOND hop.
    assert!(log[1].contains("GET /after COOKIE:hop=1 "), "got {:?}", log[1]);
  }

  #[tokio::test]
  async fn post_303_rewrites_to_bodyless_get() {
    let (base, log) = spawn_server(|path| {
      if path == "/submit" {
        (303, vec![("Location".into(), "/done".into())], "")
      } else {
        (200, vec![], "done")
      }
    });
    let bridge = MockBridge::new();
    let client = HttpClient::context_bound(bridge);
    let resp = client
      .post(
        &format!("{base}/submit"),
        Some(RequestOptions {
          json_data: Some(serde_json::json!({"a": 1})),
          ..Default::default()
        }),
      )
      .await
      .unwrap();
    assert_eq!(resp.status(), 200);
    let log = log.lock().unwrap();
    assert!(log[0].starts_with("POST /submit"), "got {:?}", log[0]);
    assert!(log[0].contains("CT:application/json"), "got {:?}", log[0]);
    // 303 → GET with the body and its content headers dropped.
    assert!(log[1].starts_with("GET /done"), "got {:?}", log[1]);
    assert!(log[1].contains("CT: CL:0"), "got {:?}", log[1]);
  }

  #[tokio::test]
  async fn explicit_cookie_header_applies_to_first_hop_only() {
    let (base, log) = spawn_server(|path| {
      if path == "/hop" {
        (302, vec![("Location".into(), "/after".into())], "")
      } else {
        (200, vec![], "ok")
      }
    });
    let bridge = MockBridge::new();
    bridge.seed(cookie("ctx", "1", "127.0.0.1"));
    let client = HttpClient::context_bound(Arc::clone(&bridge) as Arc<dyn ContextBridge>);
    client
      .get(
        &format!("{base}/hop"),
        Some(RequestOptions {
          headers: Some(vec![("cookie".into(), "manual=1".into())]),
          ..Default::default()
        }),
      )
      .await
      .unwrap();
    let log = log.lock().unwrap();
    // First hop: the caller's explicit header, untouched.
    assert!(log[0].contains("COOKIE:manual=1 "), "got {:?}", log[0]);
    // Redirect hop: re-derived from the context (Playwright drops the
    // explicit header on redirects).
    assert!(log[1].contains("COOKIE:ctx=1 "), "got {:?}", log[1]);
  }

  #[tokio::test]
  async fn max_redirects_zero_returns_the_redirect() {
    let (base, _log) = spawn_server(|_| (302, vec![("Location".into(), "/next".into())], ""));
    let client = HttpClient::context_bound(MockBridge::new());
    let resp = client
      .get(
        &format!("{base}/hop"),
        Some(RequestOptions {
          max_redirects: Some(0),
          ..Default::default()
        }),
      )
      .await
      .unwrap();
    assert_eq!(resp.status(), 302);
  }

  #[tokio::test]
  async fn redirect_loop_errors_at_budget() {
    let (base, _log) = spawn_server(|_| (302, vec![("Location".into(), "/again".into())], ""));
    let client = HttpClient::context_bound(MockBridge::new());
    let err = client
      .get(
        &format!("{base}/hop"),
        Some(RequestOptions {
          max_redirects: Some(3),
          ..Default::default()
        }),
      )
      .await
      .unwrap_err();
    assert!(err.to_string().contains("too many redirects"), "got {err}");
  }

  #[tokio::test]
  async fn base_url_and_extra_headers_come_from_bridge_defaults() {
    let (base, log) = spawn_server(|_| (200, vec![], "ok"));
    let bridge = Arc::new(MockBridge {
      defaults: ContextDefaults {
        base_url: Some(base.clone()),
        extra_http_headers: vec![("x-ctx".into(), "live".into())],
        user_agent: Some("FerriUA/9.9".into()),
        ignore_https_errors: false,
      },
      cookies: StdMutex::new(Vec::new()),
    });
    let client = HttpClient::context_bound(bridge);
    let resp = client.get("/relative", None).await.unwrap();
    assert_eq!(resp.status(), 200);
    let log = log.lock().unwrap();
    assert!(log[0].starts_with("GET /relative"), "got {:?}", log[0]);
    assert!(log[0].contains("UA:FerriUA/9.9"), "got {:?}", log[0]);
    assert!(log[0].contains("XCTX:live"), "got {:?}", log[0]);
  }
}
