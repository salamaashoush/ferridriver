//! The runner-side HTTP client — the Playwright `request` adapter over
//! the `ferrijs_fetch` engine, separate from the browser/page network.
//! Backs both the `fetch` global and the Playwright-style `request`
//! binding, which lower into the same [`fetch::Request`] and go through
//! the same [`fetch::send`] path.
//!
//! [`HttpClient`] provides `get`, `post`, `put`, `delete`, `patch`,
//! `head`, and generic `fetch` (buffered), plus [`HttpClient::send_whatwg`]
//! for streaming WHATWG `fetch` backends.
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

use ferrijs_fetch as fetch;

use crate::options::{HttpCredentials, HttpCredentialsSend, ProxyConfig};

pub use ferrijs_fetch::{
  BridgeFuture, ContextBridge, ContextDefaults, Credentials, FetchError, MultipartField, MultipartValue, NetGuard,
  NetPolicy, RedirectMode, RemoteAddr, ResponseType, multipart_boundary, multipart_boundary_of, parse_multipart,
  serialize_multipart,
};

mod response;
#[cfg(test)]
mod tests;

pub use response::HttpResponse;

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
  /// `userAgent` every request sends unless it names its own.
  pub user_agent: Option<String>,
  /// Fail a request answered 4xx or 5xx unless the request says otherwise.
  pub fail_on_status_code: Option<bool>,
  /// Redirects a request follows unless it says otherwise.
  pub max_redirects: Option<u32>,
  /// Playwright `proxy`: an HTTP(S) or SOCKS proxy every request goes
  /// through, credentials included.
  pub proxy: Option<ProxyConfig>,
}

/// The credentials a context-bound client answers challenges with, read
/// when one is needed so `setHTTPCredentials` after the client was made
/// still counts.
pub type LiveHttpCredentials = Arc<dyn Fn() -> futures::future::BoxFuture<'static, Vec<HttpCredentials>> + Send + Sync>;

/// The proxy a context-bound client sends through: the context's own, or
/// the browser's launch proxy. Read once, on the first request, since a
/// context's proxy is fixed when the context is made.
pub type LiveProxy = Arc<dyn Fn() -> futures::future::BoxFuture<'static, Option<ProxyConfig>> + Send + Sync>;

/// Type-erases `fetch::ClientPool` so proving `HttpClient: Send + Sync` stops
/// here instead of walking ~80 layers of `reqwest`/`tower`/`hyper`/`rustls`
/// types across every downstream future that holds `Page` or `RegisteredRoute`.
trait DynClientPool: Send + Sync {
  fn as_pool(&self) -> &fetch::ClientPool;
}

impl DynClientPool for fetch::ClientPool {
  fn as_pool(&self) -> &fetch::ClientPool {
    self
  }
}

#[derive(Clone)]
struct ErasedClientPool(Arc<dyn DynClientPool>);

impl ErasedClientPool {
  fn new(pool: fetch::ClientPool) -> Self {
    Self(Arc::new(pool))
  }

  fn as_pool(&self) -> &fetch::ClientPool {
    self.0.as_pool()
  }
}

/// Type-erases [`crate::context::ContextRef`] inside [`HttpClient`] so
/// proving `HttpClient: Send + Sync` stops here instead of walking the
/// `ContextRef -> BrowserState -> Browser -> Emitter<BrowserEvent>` cycle
/// in downstream crates like `ferridriver-mcp`.
trait ContextHandle: Send + Sync {
  fn storage_state(&self) -> futures::future::BoxFuture<'_, crate::error::Result<crate::options::StorageState>>;
  fn cookies(&self) -> futures::future::BoxFuture<'_, crate::error::Result<Vec<crate::backend::CookieData>>>;
  fn add_cookies(
    &self,
    cookies: Vec<crate::backend::CookieData>,
  ) -> futures::future::BoxFuture<'_, crate::error::Result<()>>;
  fn clear_cookies<'a>(
    &'a self,
    options: &'a crate::backend::ClearCookieOptions,
  ) -> futures::future::BoxFuture<'a, crate::error::Result<()>>;
}

impl ContextHandle for crate::context::ContextRef {
  fn storage_state(&self) -> futures::future::BoxFuture<'_, crate::error::Result<crate::options::StorageState>> {
    Box::pin(self.storage_state_impl(None))
  }

  fn cookies(&self) -> futures::future::BoxFuture<'_, crate::error::Result<Vec<crate::backend::CookieData>>> {
    Box::pin(self.cookies())
  }

  fn add_cookies(
    &self,
    cookies: Vec<crate::backend::CookieData>,
  ) -> futures::future::BoxFuture<'_, crate::error::Result<()>> {
    Box::pin(self.add_cookies(cookies))
  }

  fn clear_cookies<'a>(
    &'a self,
    options: &'a crate::backend::ClearCookieOptions,
  ) -> futures::future::BoxFuture<'a, crate::error::Result<()>> {
    Box::pin(self.clear_cookies_impl(options))
  }
}

#[derive(Clone)]
struct ContextProxy {
  resolve: LiveProxy,
  pool: Arc<tokio::sync::OnceCell<ErasedClientPool>>,
}

fn basic_authorization(credentials: &HttpCredentials) -> String {
  use base64::Engine as _;
  let pair = format!("{}:{}", credentials.username, credentials.password);
  format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair))
}

/// Refuse a proxy that `guard` would not let a request reach. The proxy is a
/// host the client connects to like any other, and a sandboxed caller must
/// not reach through one what its policy keeps it from reaching directly.
///
/// # Errors
///
/// The settings name no proxy, or the policy refuses the proxy's host.
pub fn check_proxy(proxy: &ProxyConfig, guard: &NetGuard) -> crate::error::Result<()> {
  proxy.validate()?;
  let server = proxy.server.trim();
  let written = if server.contains("://") {
    server.to_string()
  } else {
    format!("http://{server}")
  };
  let parsed = reqwest::Url::parse(&written)
    .map_err(|e| crate::error::FerriError::invalid_argument("proxy", format!("{server:?}: {e}")))?;
  let host = parsed.host_str().unwrap_or_default();
  let port = parsed.port().unwrap_or_else(|| match parsed.scheme() {
    "https" => 443,
    "http" => 80,
    _ => 1080,
  });
  let probe = reqwest::Url::parse(&format!("http://{host}:{port}/"))
    .map_err(|e| crate::error::FerriError::invalid_argument("proxy", format!("{server:?}: {e}")))?;
  fetch::check_url(&probe, guard).map_err(|e| crate::error::FerriError::backend(fetch::FetchError::from(e).to_string()))
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

/// The headers of a captured request that may be sent again: those that
/// describe the connection rather than the request go, since the client
/// recomputes them. `content-length` in particular must: a replay whose
/// body the capture did not carry would otherwise announce a length the
/// server then waits forever to receive.
#[must_use]
pub fn replayable_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
  headers
    .into_iter()
    .filter(|(name, _)| {
      !matches!(
        name.to_ascii_lowercase().as_str(),
        "content-length" | "host" | "connection" | "transfer-encoding"
      )
    })
    .collect()
}

/// A request assembled by the WHATWG `fetch` layer, for
/// [`HttpClient::send_whatwg`].
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

/// State owned by a standalone [`HttpClient`] (`request.newContext()` /
/// `HttpClient::new`). Holds its own reqwest cookie jar, fixed options,
/// credentials, and initial `storageState` origins.
#[derive(Clone)]
struct StandaloneState {
  pool: ErasedClientPool,
  base_url: Option<String>,
  extra_headers: Vec<(String, String)>,
  ignore_https_errors: bool,
  credentials: Vec<HttpCredentials>,
  user_agent: Option<String>,
  fail_on_status_code: Option<bool>,
  max_redirects: Option<u32>,
  /// The `origins` of the `storageState` a standalone client was made
  /// with, handed back by [`HttpClient::storage_state`].
  origins: Vec<crate::options::OriginState>,
}

/// State owned by a browser-context-bound [`HttpClient`] (`page.request` /
/// `context.request`). Cookies live in the browser via `bridge` and
/// `context`; defaults, credentials and proxy are read live from the
/// context rather than copied at construction time.
#[derive(Clone)]
struct ContextBoundState {
  base_pool: ErasedClientPool,
  bridge: Arc<dyn ContextBridge>,
  context: Option<Arc<dyn ContextHandle>>,
  credentials: Option<LiveHttpCredentials>,
  proxy: Option<ContextProxy>,
}

/// Discriminates standalone clients (own jar + fixed options) from
/// browser-context-bound clients (browser jar + live context bridge) so
/// mode-specific invariants are enforced by the type system.
#[derive(Clone)]
enum ClientMode {
  Standalone(StandaloneState),
  ContextBound(ContextBoundState),
}

/// A general HTTP client: all methods, JSON/form/multipart bodies,
/// query params, custom headers, timeouts, and cookie persistence. The
/// one stack `fetch` and `request` share.
#[derive(Clone)]
pub struct HttpClient {
  mode: ClientMode,
  default_timeout: Duration,
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
      cookie.path = path
        .rsplit_once('/')
        .map_or_else(|| "/".to_string(), |(dir, _)| format!("{dir}/"));
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

/// `failOnStatusCode`: Playwright throws on anything but 2xx and 3xx.
fn fails_status(status: u16) -> bool {
  !(200..400).contains(&status)
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
      mode: ClientMode::Standalone(StandaloneState {
        pool: ErasedClientPool::new(
          fetch::ClientPool::standalone(options.ignore_https_errors)
            .with_proxy(options.proxy.as_ref().map(ProxyConfig::transport)),
        ),
        base_url: options.base_url,
        extra_headers: options.extra_http_headers,
        ignore_https_errors: options.ignore_https_errors,
        credentials: options.http_credentials,
        user_agent: options.user_agent,
        fail_on_status_code: options.fail_on_status_code,
        max_redirects: options.max_redirects,
        origins: Vec::new(),
      }),
      default_timeout: options.timeout.unwrap_or(Duration::from_secs(30)),
    }
  }

  /// Create a validated standalone HTTP client, optionally seeded from a
  /// `storageState`. Validates `options.proxy` up front so both the NAPI
  /// and `QuickJS` bindings report the same error before any request runs.
  ///
  /// # Errors
  ///
  /// Returns an error if `options.proxy` is invalid or if `storage_state`
  /// contains a malformed cookie.
  pub fn try_new(
    options: HttpClientOptions,
    storage_state: Option<crate::options::StorageState>,
  ) -> crate::error::Result<Self> {
    if let Some(proxy) = &options.proxy {
      proxy.validate()?;
    }
    let client = Self::new(options);
    match storage_state {
      Some(state) => client.with_storage_state(state),
      None => Ok(client),
    }
  }

  /// Seed a standalone client from Playwright's `storageState`: its
  /// cookies fill the jar, its origins come back from [`Self::storage_state`].
  ///
  /// # Errors
  ///
  /// A cookie with neither a `url` nor a `domain`/`path` pair.
  pub fn with_storage_state(mut self, state: crate::options::StorageState) -> crate::error::Result<Self> {
    if let ClientMode::Standalone(standalone) = &mut self.mode {
      if let Some(jar) = standalone.pool.as_pool().jar() {
        let cookies = state
          .cookies
          .into_iter()
          .map(stored_cookie)
          .collect::<crate::error::Result<Vec<_>>>()?;
        jar.add(cookies);
      }
      standalone.origins = state.origins;
    }
    Ok(self)
  }

  /// Playwright: `apiRequestContext.storageState()`. The context's own
  /// for a bound client; this client's cookies and the origins it was
  /// made with otherwise.
  ///
  /// # Errors
  ///
  /// The bound context's state cannot be read.
  pub async fn storage_state(&self) -> crate::error::Result<crate::options::StorageState> {
    match &self.mode {
      ClientMode::ContextBound(bound) => {
        if let Some(context) = &bound.context {
          return context.storage_state().await;
        }
        Ok(crate::options::StorageState {
          cookies: Vec::new(),
          origins: Vec::new(),
          credentials: None,
        })
      },
      ClientMode::Standalone(standalone) => Ok(crate::options::StorageState {
        cookies: standalone.pool.as_pool().jar().map(|jar| jar.all()).unwrap_or_default(),
        origins: standalone.origins.clone(),
        credentials: None,
      }),
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
      mode: ClientMode::ContextBound(ContextBoundState {
        base_pool: ErasedClientPool::new(fetch::ClientPool::bridged()),
        bridge,
        context: None,
        credentials: None,
        proxy: None,
      }),
      default_timeout: Duration::from_secs(30),
    }
  }

  /// The context whose cookies `cookies()` / `add_cookies()` /
  /// `clear_cookies()` read and write, for a client bound to one.
  #[must_use]
  pub fn with_context(mut self, context: crate::context::ContextRef) -> Self {
    if let ClientMode::ContextBound(bound) = &mut self.mode {
      bound.context = Some(Arc::new(context));
    }
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
    let all = match &self.mode {
      ClientMode::ContextBound(bound) => match &bound.context {
        Some(context) => match context.cookies().await {
          Ok(cookies) => cookies,
          // A context has no jar in the browser before its first page.
          Err(crate::error::FerriError::InvalidArgument { .. }) => Vec::new(),
          Err(e) => return Err(e),
        },
        None => Vec::new(),
      },
      ClientMode::Standalone(standalone) => standalone.pool.as_pool().jar().map(|jar| jar.all()).unwrap_or_default(),
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
    match &self.mode {
      ClientMode::ContextBound(bound) => match &bound.context {
        Some(context) => context.add_cookies(cookies).await,
        None => Ok(()),
      },
      ClientMode::Standalone(standalone) => {
        if let Some(jar) = standalone.pool.as_pool().jar() {
          let cookies = cookies
            .into_iter()
            .map(stored_cookie)
            .collect::<crate::error::Result<Vec<_>>>()?;
          jar.add(cookies);
        }
        Ok(())
      },
    }
  }

  /// Playwright: `apiRequestContext.clearCookies(options?)`. Without a
  /// filter every cookie goes; with one, those matching all of it.
  ///
  /// # Errors
  ///
  /// Returns an error if a filter's regex does not compile, or if the
  /// context's cookies cannot be cleared.
  #[track_caller]
  pub fn clear_cookies(&self) -> crate::action::Action<'_, crate::backend::ClearCookieOptions, ()> {
    crate::action::Action::new(move |opts| Box::pin(async move { self.clear_cookies_impl(&opts).await }))
  }

  async fn clear_cookies_impl(&self, options: &crate::backend::ClearCookieOptions) -> crate::error::Result<()> {
    match &self.mode {
      ClientMode::ContextBound(bound) => match &bound.context {
        Some(context) => context.clear_cookies(options).await,
        None => Ok(()),
      },
      ClientMode::Standalone(standalone) => {
        if let Some(jar) = standalone.pool.as_pool().jar() {
          let filter = options.compile()?;
          jar.remove(|cookie| filter.matches(cookie));
        }
        Ok(())
      },
    }
  }

  /// Answer challenges with whatever `credentials` returns at the time.
  #[must_use]
  pub fn with_live_http_credentials(mut self, credentials: LiveHttpCredentials) -> Self {
    if let ClientMode::ContextBound(bound) = &mut self.mode {
      bound.credentials = Some(credentials);
    }
    self
  }

  /// Send through whatever proxy `resolve` returns when the first request
  /// is made, as a context's `request` sends through the context's proxy.
  #[must_use]
  pub fn with_live_proxy(mut self, resolve: LiveProxy) -> Self {
    if let ClientMode::ContextBound(bound) = &mut self.mode {
      bound.proxy = Some(ContextProxy {
        resolve,
        pool: Arc::new(tokio::sync::OnceCell::new()),
      });
    }
    self
  }

  fn bridge(&self) -> Option<&Arc<dyn ContextBridge>> {
    match &self.mode {
      ClientMode::ContextBound(bound) => Some(&bound.bridge),
      ClientMode::Standalone(_) => None,
    }
  }

  fn default_max_redirects(&self) -> Option<u32> {
    match &self.mode {
      ClientMode::Standalone(standalone) => standalone.max_redirects,
      ClientMode::ContextBound(_) => None,
    }
  }

  fn default_fail_on_status_code(&self) -> Option<bool> {
    match &self.mode {
      ClientMode::Standalone(standalone) => standalone.fail_on_status_code,
      ClientMode::ContextBound(_) => None,
    }
  }

  async fn transport_pool(&self) -> fetch::ClientPool {
    match &self.mode {
      ClientMode::Standalone(standalone) => standalone.pool.as_pool().clone(),
      ClientMode::ContextBound(bound) => {
        let Some(live) = &bound.proxy else {
          return bound.base_pool.as_pool().clone();
        };
        live
          .pool
          .get_or_init(|| async {
            let proxy = (live.resolve)().await;
            ErasedClientPool::new(
              bound
                .base_pool
                .as_pool()
                .clone()
                .with_proxy(proxy.as_ref().map(ProxyConfig::transport)),
            )
          })
          .await
          .as_pool()
          .clone()
      },
    }
  }

  async fn http_credentials(&self) -> Vec<HttpCredentials> {
    match &self.mode {
      ClientMode::Standalone(standalone) => standalone.credentials.clone(),
      ClientMode::ContextBound(bound) => match &bound.credentials {
        Some(read) => read().await,
        None => Vec::new(),
      },
    }
  }

  /// Send a prepared request over this client's transport pool.
  async fn send_prepared(&self, request: fetch::Request) -> crate::error::Result<fetch::Response> {
    let pool = self.transport_pool().await;
    fetch::send(&pool, self.bridge(), request)
      .await
      .map_err(|e| crate::error::FerriError::backend(e.to_string()))
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
    match &self.mode {
      ClientMode::ContextBound(bound) => {
        let d = bound
          .bridge
          .defaults()
          .await
          .map_err(|e| crate::error::FerriError::backend(e.to_string()))?;
        Ok(ResolvedDefaults {
          base_url: d.base_url,
          extra_headers: d.extra_http_headers,
          user_agent: d.user_agent,
          ignore_https_errors: d.ignore_https_errors,
          ctx_style: true,
        })
      },
      ClientMode::Standalone(standalone) => Ok(ResolvedDefaults {
        base_url: standalone.base_url.clone(),
        extra_headers: standalone.extra_headers.clone(),
        user_agent: standalone.user_agent.clone(),
        ignore_https_errors: standalone.ignore_https_errors,
        ctx_style: false,
      }),
    }
  }

  /// The default + context headers every request starts from, before the
  /// caller's own.
  fn base_headers(defaults: &ResolvedDefaults) -> fetch::Headers {
    let mut headers = fetch::Headers::new();
    if let Some(ua) = &defaults.user_agent {
      headers.set("user-agent", ua.clone());
    }
    if defaults.ctx_style {
      headers.set("accept", "*/*");
    }
    for (name, value) in &defaults.extra_headers {
      headers.set(name, value.clone());
    }
    headers
  }

  /// Send a request assembled by the WHATWG `fetch` layer, answering with
  /// the engine's own [`fetch::Response`] so the caller can stream the body
  /// directly (backs the runtime's `fetch` global over this client).
  ///
  /// The caller has already run the spec's "extract a body" step, so the
  /// body and its `content-type` arrive decided. That is the whole point
  /// of this entry point: `build_request` applies Playwright's
  /// `data` defaults (notably `content-type: application/octet-stream`),
  /// which are right for `request.post(url, { data })` and wrong for
  /// `fetch(url, { body: someArrayBuffer })` — where the spec sends no
  /// `content-type` at all.
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

    let pool = self.transport_pool().await;
    fetch::send(
      &pool,
      self.bridge(),
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
  /// the client options), reading credentials once for both `send: always`
  /// and 401 Basic challenges, and materializing the body without copying.
  async fn build_request(&self, url: &str, opts: RequestOptions) -> crate::error::Result<fetch::Request> {
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
    if let Some(request_headers) = opts.headers {
      for (k, v) in request_headers {
        headers.set(&k, v);
      }
    }

    // Read credentials once for both `send: 'always'` (applied after the
    // request's own headers, matching fetch.ts) and per-hop 401 Basic
    // challenges (including cross-origin redirects).
    let credentials = self.http_credentials().await;
    if let Some(entry) = crate::options::matching_http_credentials(&credentials, request_url.as_str())
      && entry.send == Some(HttpCredentialsSend::Always)
    {
      headers.set("authorization", basic_authorization(entry));
    }
    let challenge = (!credentials.is_empty()).then(|| {
      fetch::ChallengeAnswer::new(move |url| {
        crate::options::matching_http_credentials(&credentials, url.as_str()).map(basic_authorization)
      })
    });

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
    } else if let Some(data) = opts.data {
      headers.set_if_absent("content-type", "application/octet-stream");
      fetch::Body::from_bytes(data)
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
      max_redirects: opts.max_redirects.or_else(|| self.default_max_redirects()),
      max_retries: opts.max_retries.unwrap_or(0),
      timeout: opts.timeout.unwrap_or(self.default_timeout),
      ignore_https_errors: opts.ignore_https_errors.unwrap_or(ignore_default),
      net_guard: opts.net_guard,
      challenge,
    })
  }

  /// Playwright: `get(url: string, options?)`. Options chain as setters
  /// on the returned action (`.header(..)`, `.json(..)`, `.params(..)`,
  /// `.timeout(..)`, ...).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  #[track_caller]
  pub fn get(&self, url: &str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    self.verb(url, "GET")
  }

  /// Playwright: `post(url: string, options?)`. Options chain as setters
  /// on the returned action (`.header(..)`, `.json(..)`, `.params(..)`,
  /// `.timeout(..)`, ...).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  #[track_caller]
  pub fn post(&self, url: &str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    self.verb(url, "POST")
  }

  /// Playwright: `put(url: string, options?)`. Options chain as setters
  /// on the returned action (`.header(..)`, `.json(..)`, `.params(..)`,
  /// `.timeout(..)`, ...).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  #[track_caller]
  pub fn put(&self, url: &str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    self.verb(url, "PUT")
  }

  /// Playwright: `delete(url: string, options?)`. Options chain as setters
  /// on the returned action (`.header(..)`, `.json(..)`, `.params(..)`,
  /// `.timeout(..)`, ...).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  #[track_caller]
  pub fn delete(&self, url: &str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    self.verb(url, "DELETE")
  }

  /// Playwright: `patch(url: string, options?)`. Options chain as setters
  /// on the returned action (`.header(..)`, `.json(..)`, `.params(..)`,
  /// `.timeout(..)`, ...).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  #[track_caller]
  pub fn patch(&self, url: &str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    self.verb(url, "PATCH")
  }

  /// Playwright: `head(url: string, options?)`. Options chain as setters
  /// on the returned action (`.header(..)`, `.json(..)`, `.params(..)`,
  /// `.timeout(..)`, ...).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  #[track_caller]
  pub fn head(&self, url: &str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    self.verb(url, "HEAD")
  }

  #[track_caller]
  fn verb(&self, url: &str, method: &'static str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    let url = url.to_string();
    crate::action::Action::new(move |opts: RequestOptions| {
      Box::pin(async move { self.fetch_impl(&url, with_method(Some(opts), method)).await })
    })
  }

  /// Send an HTTP request (generic method — all verbs delegate here).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or `fail_on_status_code` is set and the response is 4xx/5xx.
  #[track_caller]
  pub fn fetch(&self, url: &str) -> crate::action::Action<'_, RequestOptions, HttpResponse> {
    let url = url.to_string();
    crate::action::Action::new(move |opts: RequestOptions| Box::pin(async move { self.fetch_impl(&url, opts).await }))
  }

  async fn fetch_impl(&self, url: &str, opts: RequestOptions) -> crate::error::Result<HttpResponse> {
    let fail_on_status_code = opts
      .fail_on_status_code
      .or_else(|| self.default_fail_on_status_code())
      .unwrap_or(false);
    let request = self.build_request(url, opts).await?;
    let method = request.method.clone();
    let response = self.send_prepared(request).await?;
    let http = HttpResponse::buffer(response).await?;
    if fail_on_status_code && fails_status(http.status()) {
      return Err(crate::error::FerriError::Backend(format!(
        "{} {} failed: {} {}",
        method,
        http.url(),
        http.status(),
        http.status_text()
      )));
    }
    Ok(http)
  }

  /// Dispose the request context (Playwright compat).
  pub fn dispose(self) {
    drop(self);
  }
}

impl RequestOptions {
  /// Method, replayable headers and raw body carried over from a captured
  /// page-network [`crate::network::Request`], for Playwright's
  /// `apiRequestContext.fetch(request, options?)`.
  #[must_use]
  pub fn from_page_request(request: &crate::network::Request) -> Self {
    Self {
      method: Some(request.method().to_string()),
      headers: Some(replayable_headers(request.headers().into_iter().collect())),
      data: request.post_data_buffer(),
      ..Self::default()
    }
  }

  /// Layer explicit `overrides` over a `Request`-derived `base`: anything
  /// the caller set wins, the rest falls through to `base`. A body given
  /// in any form replaces `base`'s body completely.
  #[must_use]
  pub fn merge_over(base: Option<Self>, overrides: Option<Self>) -> Option<Self> {
    let (base, options) = match (base, overrides) {
      (Some(base), Some(options)) => (base, options),
      (base, options) => return base.or(options),
    };
    Some(Self {
      method: options.method.or(base.method),
      headers: match (options.headers, base.headers) {
        (Some(explicit), Some(inherited)) => {
          let mut merged = explicit;
          for (name, value) in inherited {
            if !merged.iter().any(|(k, _)| k.eq_ignore_ascii_case(&name)) {
              merged.push((name, value));
            }
          }
          Some(merged)
        },
        (explicit, inherited) => explicit.or(inherited),
      },
      data: options.data.or_else(|| {
        (options.json_data.is_none() && options.form.is_none() && options.multipart.is_none()).then_some(base.data)?
      }),
      ..options
    })
  }

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
