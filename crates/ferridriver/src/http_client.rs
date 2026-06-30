//! WHATWG-fetch-compatible HTTP client -- the runner-side request
//! stack, separate from the browser/page network. Backs both the
//! `fetch` global and the Playwright-style `request` binding.
//!
//! Provides `HttpClient` with `get`, `post`, `put`, `delete`, `patch`,
//! `head`, and generic `fetch`.
//!
//! Each method returns an `HttpResponse` with `status()`, `text()`, `json()`,
//! `headers()`, `ok()`, and `body()`.
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

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustc_hash::FxHashMap;

// ── Sandbox network guard (SSRF defense) ──────────────────────────────
//
// The scripting sandbox stops disk/process escape; the network was the
// remaining hole. `NetGuard` is enforced in core (Rust source of truth)
// so the `request` binding, the global `fetch`, and every plugin
// `allow.net` tool share one implementation:
//
//  * a per-hop host allow-list — checked on the initial URL AND on every
//    redirect target, so an allowed host can no longer 302 a
//    net-restricted caller into an internal address;
//  * a DNS filter that drops cloud-metadata / (optionally) private
//    resolved addresses, which also defeats DNS rebinding (a public
//    hostname that resolves to 169.254.169.254);
//  * scheme pinning (http/https only).
//
// Default sandbox posture blocks the cloud-metadata endpoints for every
// script `fetch`/`request` (no legitimate automation targets them),
// while loopback/private stays reachable so local test servers keep
// working unless an operator opts in.

/// Boxed error for the custom DNS resolver (`reqwest::dns::Resolving`
/// resolves to `Result<Addrs, BoxError>`).
type BoxErr = Box<dyn std::error::Error + Send + Sync>;

/// Per-request network policy for the scripting sandbox. `Default`
/// (all-false / no allow-list) is inert — non-sandbox callers never set
/// it and keep the original cached-client fast path untouched.
#[derive(Debug, Clone, Default)]
pub struct NetGuard {
  /// Host allow-list (plugin `allow.net`). `None` ⇒ unrestricted host;
  /// `Some` ⇒ default-deny, enforced on the initial URL and every
  /// redirect hop.
  pub allowlist: Option<Arc<[String]>>,
  /// Block the cloud instance-metadata endpoints (169.254.169.254 /
  /// `fd00:ec2::254`) at both the URL and the resolved-address layer.
  pub block_metadata: bool,
  /// Also block loopback / RFC1918 / link-local / ULA / CGNAT. Off by
  /// default so local automation against `127.0.0.1` test servers still
  /// works; an operator opts in.
  pub block_private: bool,
}

impl NetGuard {
  /// Whether this guard changes behaviour at all. When `false` the
  /// caller uses the unguarded cached-client path (zero overhead).
  #[must_use]
  pub fn is_active(&self) -> bool {
    self.allowlist.is_some() || self.block_metadata || self.block_private
  }

  /// Stable key for the guarded-client cache: identical guards reuse one
  /// reqwest `Client` (so the common sandbox path — no allow-list,
  /// metadata blocked — is a single shared client, not one per request).
  fn cache_key(&self, max_redirects: Option<u32>) -> String {
    let mut list = self.allowlist.as_deref().map(<[String]>::to_vec).unwrap_or_default();
    list.sort();
    format!(
      "{}|{}|{}|{}",
      list.join(","),
      u8::from(self.block_metadata),
      u8::from(self.block_private),
      max_redirects.map_or_else(|| "-".to_string(), |m| m.to_string())
    )
  }
}

/// Extract the lowercased host (no port, no userinfo) from an absolute
/// URL. `None` for relative/invalid input — callers treat that as a
/// denial when an allow-list is active (fail closed).
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
  let after_scheme = url.split_once("://")?.1;
  let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or(after_scheme);
  let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
  let host = if let Some(stripped) = host_port.strip_prefix('[') {
    stripped.split(']').next().unwrap_or(stripped)
  } else {
    host_port.split(':').next().unwrap_or(host_port)
  };
  (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Match a host against one allow-list entry set: exact, or a
/// leading-wildcard suffix (`*.box.com` also matches the bare apex
/// `box.com`).
#[must_use]
pub fn host_allowed(host: &str, net: &[String]) -> bool {
  net.iter().any(|p| {
    if p == host {
      return true;
    }
    if let Some(suffix) = p.strip_prefix("*.") {
      return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    false
  })
}

/// Normalize an IPv4-mapped/compatible IPv6 address down to its IPv4
/// form so range checks see the real address.
fn canon_ip(ip: IpAddr) -> IpAddr {
  match ip {
    IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
    v4 @ IpAddr::V4(_) => v4,
  }
}

/// The cloud instance-metadata addresses (AWS/GCP/Azure/OpenStack IMDS,
/// and the AWS IPv6 IMDS). These have no legitimate automation use and
/// are the canonical SSRF target, so they are blocked by default.
fn is_metadata_ip(ip: IpAddr) -> bool {
  match canon_ip(ip) {
    IpAddr::V4(v4) => v4 == Ipv4Addr::new(169, 254, 169, 254),
    IpAddr::V6(v6) => v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254),
  }
}

/// Loopback / private / link-local / ULA / CGNAT / unspecified — the
/// "internal network" set blocked when `block_private` is on.
fn is_private_ip(ip: IpAddr) -> bool {
  match canon_ip(ip) {
    IpAddr::V4(v4) => {
      v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.octets()[0] == 0
        // RFC 6598 carrier-grade NAT 100.64.0.0/10.
        || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
    },
    IpAddr::V6(v6) => {
      v6.is_loopback()
        || v6.is_unspecified()
        // Unique-local fc00::/7.
        || (v6.segments()[0] & 0xfe00) == 0xfc00
        // Link-local fe80::/10.
        || (v6.segments()[0] & 0xffc0) == 0xfe80
    },
  }
}

/// `true` if the address must not be connected to under this guard.
fn ip_blocked(ip: IpAddr, block_metadata: bool, block_private: bool) -> bool {
  (block_metadata && is_metadata_ip(ip)) || (block_private && is_private_ip(ip))
}

/// Validate one concrete URL (initial or a redirect target) against the
/// guard: scheme must be http/https, a literal-IP host is range-checked,
/// and the host must satisfy the allow-list. Returns the denial reason.
fn check_url(url: &reqwest::Url, g: &NetGuard) -> Result<(), String> {
  let scheme = url.scheme();
  if scheme != "http" && scheme != "https" {
    return Err(format!(
      "scheme \"{scheme}\" is not permitted by the sandbox network policy"
    ));
  }
  let host = url
    .host_str()
    .ok_or_else(|| "request to a URL with no host is not permitted".to_string())?;
  if let Ok(ip) = host.parse::<IpAddr>()
    && ip_blocked(ip, g.block_metadata, g.block_private)
  {
    return Err(format!("request to blocked address {ip} (sandbox network policy)"));
  }
  if let Some(list) = &g.allowlist
    && !host_allowed(&host.to_ascii_lowercase(), list)
  {
    return Err(format!("request host \"{host}\" is not in allow.net {list:?}"));
  }
  Ok(())
}

/// Pre-flight the initial (already base-resolved) request URL. A
/// parse failure under an active guard is a denial (fail closed).
fn preflight(resolved_url: &str, g: &NetGuard) -> Result<(), String> {
  match reqwest::Url::parse(resolved_url) {
    Ok(u) => check_url(&u, g),
    Err(_) => Err(format!(
      "request to invalid/relative URL \"{resolved_url}\" is not permitted by the sandbox network policy"
    )),
  }
}

/// Custom reqwest DNS resolver that resolves the host normally, then
/// drops any address the guard forbids. Empty after filtering ⇒ the
/// connection is refused. This is what defeats DNS rebinding: a public
/// hostname resolving to a metadata/private address never connects.
struct GuardedResolver {
  block_metadata: bool,
  block_private: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
  fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
    let host = name.as_str().to_string();
    let (bm, bp) = (self.block_metadata, self.block_private);
    Box::pin(async move {
      let lookup = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<SocketAddr>> {
        Ok((host.as_str(), 0u16).to_socket_addrs()?.collect())
      })
      .await;
      let addrs = match lookup {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => return Err(Box::new(e) as BoxErr),
        Err(e) => return Err(Box::new(e) as BoxErr),
      };
      let kept: Vec<SocketAddr> = addrs.into_iter().filter(|sa| !ip_blocked(sa.ip(), bm, bp)).collect();
      if kept.is_empty() {
        return Err("all resolved addresses blocked by sandbox network policy".into());
      }
      Ok(Box::new(kept.into_iter()) as reqwest::dns::Addrs)
    })
  }
}

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
}

/// Per-request options (overrides context defaults).
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
  /// Query string parameters.
  pub params: Option<Vec<(String, String)>>,
  /// Per-request timeout override.
  pub timeout: Option<Duration>,
  /// Fail with error on 4xx/5xx status codes.
  pub fail_on_status_code: Option<bool>,
  /// Per-request redirect cap: `Some(0)` does not follow redirects
  /// (the 3xx is returned as-is), `Some(n)` follows up to `n` then
  /// errors, `None` uses the client default.
  pub max_redirects: Option<u32>,
  /// Sandbox network policy. `None`/inert ⇒ the original unguarded
  /// cached-client path. `Some(active)` enforces the allow-list +
  /// metadata/private/scheme rules on the initial URL, every redirect
  /// hop, and every resolved address.
  pub net_guard: Option<NetGuard>,
}

/// Resolved peer address of a response. Mirrors Playwright's
/// `RemoteAddr` (`{ ipAddress, port }`) returned by
/// `apiResponse.serverAddr()` / `response.serverAddr()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteAddr {
  #[serde(rename = "ipAddress")]
  pub ip_address: String,
  pub port: u16,
}

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
  status_code: u16,
  status_text: String,
  response_url: String,
  response_headers: Vec<(String, String)>,
  body_bytes: bytes::Bytes,
  server_addr: Option<RemoteAddr>,
}

impl HttpResponse {
  /// HTTP status code.
  pub fn status(&self) -> u16 {
    self.status_code
  }

  /// HTTP status text (e.g., "OK", "Not Found").
  pub fn status_text(&self) -> &str {
    &self.status_text
  }

  /// Final URL after redirects.
  pub fn url(&self) -> &str {
    &self.response_url
  }

  /// Whether the response status is 200-299.
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status_code)
  }

  /// Response headers as (name, value) pairs.
  pub fn headers(&self) -> &[(String, String)] {
    &self.response_headers
  }

  /// Get a specific header value by name (case-insensitive).
  pub fn header(&self, name: &str) -> Option<&str> {
    let lower = name.to_lowercase();
    self
      .response_headers
      .iter()
      .find(|(k, _)| k.to_lowercase() == lower)
      .map(|(_, v)| v.as_str())
  }

  /// Response body as UTF-8 string.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid UTF-8.
  pub fn text(&self) -> crate::error::Result<String> {
    String::from_utf8(self.body_bytes.to_vec())
      .map_err(|e| crate::error::FerriError::evaluation(format!("response body is not UTF-8: {e}")))
  }

  /// Parse response body as JSON.
  ///
  /// # Errors
  ///
  /// Returns an error if the body cannot be deserialized.
  pub fn json<T: serde::de::DeserializeOwned>(&self) -> crate::error::Result<T> {
    serde_json::from_slice(&self.body_bytes).map_err(Into::into)
  }

  /// Response body as a JSON value.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid JSON.
  pub fn json_value(&self) -> crate::error::Result<serde_json::Value> {
    self.json()
  }

  /// Raw response body bytes.
  pub fn body(&self) -> &[u8] {
    &self.body_bytes
  }

  /// Resolved peer address (`{ ipAddress, port }`), or `None` when the
  /// transport didn't surface one. Playwright:
  /// `apiResponse.serverAddr(): Promise<RemoteAddr | null>`.
  pub fn server_addr(&self) -> Option<&RemoteAddr> {
    self.server_addr.as_ref()
  }

  /// Consume the response (Playwright compat, no-op in Rust since we own the bytes).
  pub fn dispose(self) {
    drop(self);
  }
}

/// A response whose body has NOT been buffered: status/headers are
/// available immediately, body bytes are pulled incrementally with
/// [`Self::chunk`]. Produced by [`HttpClient::fetch_stream`]; backs a
/// WHATWG `Response.body` `ReadableStream`.
#[derive(Debug)]
pub struct HttpStreamResponse {
  status_code: u16,
  status_text: String,
  response_url: String,
  response_headers: Vec<(String, String)>,
  inner: reqwest::Response,
}

impl HttpStreamResponse {
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

  #[must_use]
  pub fn headers(&self) -> &[(String, String)] {
    &self.response_headers
  }

  /// Next body chunk, or `None` at end of stream.
  ///
  /// # Errors
  ///
  /// Returns an error if reading the body fails (connection reset, etc).
  pub async fn chunk(&mut self) -> crate::error::Result<Option<bytes::Bytes>> {
    self
      .inner
      .chunk()
      .await
      .map_err(|e| crate::error::FerriError::Backend(format!("read response body: {e}")))
  }
}

/// A general HTTP client: all methods, JSON/form/multipart bodies,
/// query params, custom headers, timeouts, and cookie persistence via
/// reqwest's cookie jar. The one stack `fetch` and `request` share.
#[derive(Clone)]
pub struct HttpClient {
  client: reqwest::Client,
  base_url: Option<String>,
  extra_headers: Vec<(String, String)>,
  default_timeout: Duration,
  /// Shared cookie jar. reqwest pins the redirect policy on the
  /// `Client`, so a per-request `max_redirects` override needs a
  /// distinct `Client`; every such client is built against THIS jar so
  /// session cookies still persist across calls regardless of which
  /// redirect-policy client served a given request.
  jar: Arc<reqwest::cookie::Jar>,
  ignore_https_errors: bool,
  /// Lazily-built clients keyed by requested redirect limit (`0` =
  /// don't follow, `n` = follow up to `n`). The default-policy client
  /// is `self.client`; this only holds the per-override ones.
  redirect_clients: Arc<Mutex<FxHashMap<u32, reqwest::Client>>>,
  /// Lazily-built sandbox-guarded clients, keyed by the guard's
  /// [`NetGuard::cache_key`]. Identical guards (the common case: no
  /// allow-list, metadata blocked) reuse one client, so guarding adds
  /// no per-request client-build cost.
  guarded_clients: Arc<Mutex<FxHashMap<String, reqwest::Client>>>,
}

/// Build a reqwest client sharing `jar` (so cookies persist across the
/// default and any per-redirect-limit clients). `max_redirects`:
/// `None` keeps reqwest's default policy, `Some(0)` does not follow
/// redirects, `Some(n)` follows up to `n` (exceeding errors).
fn build_client(
  jar: &Arc<reqwest::cookie::Jar>,
  ignore_https_errors: bool,
  max_redirects: Option<u32>,
) -> reqwest::Client {
  let mut builder = reqwest::Client::builder().cookie_provider(jar.clone());
  if let Some(max) = max_redirects {
    let policy = if max == 0 {
      reqwest::redirect::Policy::none()
    } else {
      reqwest::redirect::Policy::limited(max as usize)
    };
    builder = builder.redirect(policy);
  }
  if ignore_https_errors {
    builder = builder.danger_accept_invalid_certs(true);
  }
  builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

impl HttpClient {
  /// Create a new HTTP client.
  #[must_use]
  pub fn new(options: HttpClientOptions) -> Self {
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let client = build_client(&jar, options.ignore_https_errors, None);
    let default_timeout = options.timeout.unwrap_or(Duration::from_secs(30));

    Self {
      client,
      base_url: options.base_url,
      extra_headers: options.extra_http_headers,
      default_timeout,
      jar,
      ignore_https_errors: options.ignore_https_errors,
      redirect_clients: Arc::new(Mutex::new(FxHashMap::default())),
      guarded_clients: Arc::new(Mutex::new(FxHashMap::default())),
    }
  }

  /// Build (once, then cache) the reqwest client for an active
  /// [`NetGuard`]: a custom redirect policy that re-checks the host on
  /// every hop and honours the redirect cap, plus a DNS resolver that
  /// filters blocked addresses. Shares the session cookie jar.
  fn guarded_client(&self, g: &NetGuard, max_redirects: Option<u32>) -> reqwest::Client {
    let key = g.cache_key(max_redirects);
    let mut cache = self
      .guarded_clients
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(c) = cache.get(&key) {
      return c.clone();
    }
    let mut builder = reqwest::Client::builder().cookie_provider(self.jar.clone());
    if self.ignore_https_errors {
      builder = builder.danger_accept_invalid_certs(true);
    }
    // `Some(0)` ⇒ never follow (return the 3xx); `Some(n)` ⇒ up to n
    // then error; `None` ⇒ reqwest's default of 10 then error. The host
    // check runs first so a disallowed redirect always errors, never
    // silently stops.
    let guard = g.clone();
    let limit = max_redirects.map_or(10usize, |m| m as usize);
    builder = builder.redirect(reqwest::redirect::Policy::custom(move |attempt| {
      if let Err(msg) = check_url(attempt.url(), &guard) {
        return attempt.error(std::io::Error::other(msg));
      }
      if attempt.previous().len() >= limit {
        return if limit == 0 {
          attempt.stop()
        } else {
          attempt.error(std::io::Error::other(format!("too many redirects (max {limit})")))
        };
      }
      attempt.follow()
    }));
    if g.block_metadata || g.block_private {
      builder = builder.dns_resolver(Arc::new(GuardedResolver {
        block_metadata: g.block_metadata,
        block_private: g.block_private,
      }));
    }
    let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
    cache.insert(key, client.clone());
    client
  }

  /// The reqwest client to use for a request: the default-policy one,
  /// or — when the caller pinned `max_redirects` — a jar-sharing client
  /// built for exactly that limit (built once, then cached).
  fn client_for(&self, max_redirects: Option<u32>) -> reqwest::Client {
    let Some(max) = max_redirects else {
      return self.client.clone();
    };
    let mut cache = self
      .redirect_clients
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
      .entry(max)
      .or_insert_with(|| build_client(&self.jar, self.ignore_https_errors, Some(max)))
      .clone()
  }

  /// Resolve a URL against the base URL.
  fn resolve_url(&self, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
      return url.to_string();
    }
    match &self.base_url {
      Some(base) => {
        let base = base.trim_end_matches('/');
        if url.starts_with('/') {
          format!("{base}{url}")
        } else {
          format!("{base}/{url}")
        }
      },
      None => url.to_string(),
    }
  }

  /// Send a GET request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn get(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("GET".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a POST request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn post(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("POST".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a PUT request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn put(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("PUT".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a DELETE request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn delete(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("DELETE".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a PATCH request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn patch(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("PATCH".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a HEAD request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn head(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("HEAD".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send an HTTP request (generic method — all verbs delegate here).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or `fail_on_status_code` is set and the response is 4xx/5xx.
  pub async fn fetch(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    let opts = options.unwrap_or_default();
    let (response, resolved_url, method_str) = self.send_request(url, &opts).await?;

    let status_code = response.status().as_u16();
    let status_text = response.status().canonical_reason().unwrap_or("Unknown").to_string();
    let response_url = response.url().to_string();
    let server_addr = response.remote_addr().map(|addr| RemoteAddr {
      ip_address: addr.ip().to_string(),
      port: addr.port(),
    });
    let response_headers: Vec<(String, String)> = response
      .headers()
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
      .collect();

    let body_bytes = response.bytes().await.map_err(|e| format!("read response body: {e}"))?;

    let api_response = HttpResponse {
      status_code,
      status_text,
      response_url,
      response_headers,
      body_bytes,
      server_addr,
    };

    if opts.fail_on_status_code.unwrap_or(false) && !api_response.ok() {
      return Err(crate::error::FerriError::Backend(format!(
        "{method_str} {resolved_url} failed: {} {}",
        api_response.status(),
        api_response.status_text()
      )));
    }

    Ok(api_response)
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
    let (response, resolved_url, method_str) = self.send_request(url, &opts).await?;

    let status_code = response.status().as_u16();
    let status_text = response.status().canonical_reason().unwrap_or("Unknown").to_string();
    let response_url = response.url().to_string();
    let response_headers: Vec<(String, String)> = response
      .headers()
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
      .collect();

    if opts.fail_on_status_code.unwrap_or(false) && !(200..300).contains(&status_code) {
      return Err(crate::error::FerriError::Backend(format!(
        "{method_str} {resolved_url} failed: {status_code} {status_text}"
      )));
    }

    Ok(HttpStreamResponse {
      status_code,
      status_text,
      response_url,
      response_headers,
      inner: response,
    })
  }

  /// Build and send the request shared by [`Self::fetch`] and
  /// [`Self::fetch_stream`]. Returns the unread response plus the
  /// resolved URL and method (for error messages).
  async fn send_request(
    &self,
    url: &str,
    opts: &RequestOptions,
  ) -> crate::error::Result<(reqwest::Response, String, String)> {
    let method_str = opts.method.as_deref().unwrap_or("GET").to_string();
    let method: reqwest::Method = method_str
      .parse()
      .map_err(|_| format!("invalid HTTP method: {method_str}"))?;

    let resolved_url = self.resolve_url(url);

    // Sandbox network guard: fail fast on the initial URL (clear error,
    // no client built) and route through the guarded client so the
    // policy also covers every redirect hop and resolved address.
    let client = match opts.net_guard.as_ref() {
      Some(g) if g.is_active() => {
        preflight(&resolved_url, g).map_err(crate::error::FerriError::Backend)?;
        self.guarded_client(g, opts.max_redirects)
      },
      _ => self.client_for(opts.max_redirects),
    };
    let mut builder = client.request(method, &resolved_url);

    for (k, v) in &self.extra_headers {
      builder = builder.header(k, v);
    }
    if let Some(headers) = &opts.headers {
      for (k, v) in headers {
        builder = builder.header(k, v);
      }
    }
    if let Some(params) = &opts.params {
      builder = builder.query(params);
    }
    // Request body (mutually exclusive: json, form, raw data).
    if let Some(json) = &opts.json_data {
      builder = builder.json(json);
    } else if let Some(form) = &opts.form {
      builder = builder.form(form);
    } else if let Some(data) = &opts.data {
      builder = builder.body(data.clone());
    }
    builder = builder.timeout(opts.timeout.unwrap_or(self.default_timeout));

    let response = builder
      .send()
      .await
      .map_err(|e| format!("request to {resolved_url} failed: {e}"))?;
    Ok((response, resolved_url, method_str))
  }

  /// Dispose the request context (Playwright compat).
  pub fn dispose(self) {
    drop(self);
  }
}

#[cfg(test)]
mod net_guard_tests {
  use super::*;

  #[test]
  fn host_of_ignores_userinfo_and_port() {
    assert_eq!(host_of("https://allowed.com/x").as_deref(), Some("allowed.com"));
    // userinfo must not let an attacker spoof the host.
    assert_eq!(host_of("https://allowed.com@evil.com/x").as_deref(), Some("evil.com"));
    assert_eq!(host_of("http://[::1]:8080/").as_deref(), Some("::1"));
    assert_eq!(host_of("/relative"), None);
  }

  #[test]
  fn host_allowlist_exact_and_wildcard() {
    let net = ["api.box.com".to_string(), "*.cdn.com".to_string()];
    assert!(host_allowed("api.box.com", &net));
    assert!(host_allowed("cdn.com", &net)); // apex
    assert!(host_allowed("a.cdn.com", &net));
    assert!(!host_allowed("evilcdn.com", &net));
    assert!(!host_allowed("box.com", &net));
  }

  #[test]
  fn metadata_addresses_classified() {
    assert!(is_metadata_ip("169.254.169.254".parse().unwrap()));
    // IPv4-mapped IPv6 must normalise so it cannot smuggle past.
    assert!(is_metadata_ip("::ffff:169.254.169.254".parse().unwrap()));
    assert!(is_metadata_ip("fd00:ec2::254".parse().unwrap()));
    assert!(!is_metadata_ip("93.184.216.34".parse().unwrap()));
  }

  #[test]
  fn private_ranges_classified() {
    for ip in [
      "127.0.0.1",
      "10.0.0.1",
      "192.168.1.1",
      "172.16.0.1",
      "169.254.0.1",
      "100.64.0.1",
      "::1",
      "fe80::1",
      "fc00::1",
    ] {
      assert!(is_private_ip(ip.parse().unwrap()), "{ip} should be private");
    }
    assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
  }

  #[test]
  fn check_url_blocks_metadata_by_default_keeps_loopback() {
    let g = NetGuard {
      allowlist: None,
      block_metadata: true,
      block_private: false,
    };
    assert!(check_url(&reqwest::Url::parse("http://169.254.169.254/").unwrap(), &g).is_err());
    // Loopback stays reachable so local automation/test servers work.
    assert!(check_url(&reqwest::Url::parse("http://127.0.0.1:9/").unwrap(), &g).is_ok());
    // Non-http(s) scheme rejected.
    assert!(check_url(&reqwest::Url::parse("file:///etc/passwd").unwrap(), &g).is_err());
  }

  #[test]
  fn check_url_enforces_allowlist_on_any_url() {
    let g = NetGuard {
      allowlist: Some(Arc::from(["allowed.com".to_string()])),
      block_metadata: true,
      block_private: false,
    };
    assert!(check_url(&reqwest::Url::parse("https://allowed.com/x").unwrap(), &g).is_ok());
    // This is the per-hop check that closes the redirect SSRF bypass:
    // the same function the custom redirect policy calls on every hop.
    assert!(check_url(&reqwest::Url::parse("https://evil.com/x").unwrap(), &g).is_err());
  }

  #[test]
  fn preflight_fails_closed_on_unparseable_url() {
    let g = NetGuard {
      allowlist: Some(Arc::from(["allowed.com".to_string()])),
      block_metadata: true,
      block_private: false,
    };
    assert!(preflight("not a url", &g).is_err());
  }

  #[test]
  fn inert_guard_is_not_active() {
    assert!(!NetGuard::default().is_active());
    assert!(
      NetGuard {
        block_metadata: true,
        ..Default::default()
      }
      .is_active()
    );
  }
}
