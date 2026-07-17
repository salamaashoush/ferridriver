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
/// leading-wildcard suffix (`*.acme.com` also matches the bare apex
/// `acme.com`).
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

// ── Context-bound client (Playwright `page.request` / `context.request`) ──
//
// Playwright's `BrowserContextAPIRequestContext`
// (`/tmp/playwright/packages/playwright-core/src/server/fetch.ts:649`)
// shares the browser context's cookie jar in both directions: the
// outgoing `Cookie` header is assembled from `context.cookies()` before
// every hop, and every hop's `Set-Cookie` headers are written back via
// `context.addCookies()`. reqwest's internal jar/redirects can't do
// that (cookies must live in the BROWSER, and each hop needs a fresh
// jar read), so the bridged path follows redirects manually — exactly
// like Playwright's `_sendRequest` loop.

/// Boxed future used by [`ContextBridge`] (`async fn` in traits is not
/// dyn-compatible).
pub type BridgeFuture<'a, T> =
  std::pin::Pin<Box<dyn std::future::Future<Output = crate::error::Result<T>> + Send + 'a>>;

/// Live per-request defaults sourced from the owning browser context.
/// Mirrors the subset of Playwright's `_defaultOptions()` (fetch.ts:666)
/// ferridriver's context options carry today.
#[derive(Debug, Clone, Default)]
pub struct ContextDefaults {
  pub base_url: Option<String>,
  pub extra_http_headers: Vec<(String, String)>,
  pub user_agent: Option<String>,
  pub ignore_https_errors: bool,
}

/// Two-way bridge between an [`HttpClient`] and a browser context.
/// Implemented by `ContextRef`; read live on every request so option
/// mutations (`setExtraHTTPHeaders`) and browser-side cookie changes are
/// always visible, matching Playwright's live `_defaultOptions()` read.
pub trait ContextBridge: Send + Sync {
  fn defaults(&self) -> BridgeFuture<'_, ContextDefaults>;
  fn cookies(&self) -> BridgeFuture<'_, Vec<crate::backend::CookieData>>;
  fn add_cookies(&self, cookies: Vec<crate::backend::CookieData>) -> BridgeFuture<'_, ()>;
}

/// RFC 6265 upper bound Playwright clamps cookie expiry to
/// (`server/network.ts::kMaxCookieExpiresDateInSeconds`).
const MAX_COOKIE_EXPIRES_SECONDS: f64 = 253_402_300_799.0;

/// Secure-cookie carve-out: Playwright sends `Secure` cookies over
/// plain http to localhost names (`server/network.ts::isLocalHostname`).
fn is_local_hostname(hostname: &str) -> bool {
  hostname == "localhost" || hostname.ends_with(".localhost")
}

/// RFC 6265 §5.1.3 domain-match as Playwright implements it
/// (`server/cookieStore.ts::domainMatches`): exact host, or a
/// dot-prefixed cookie domain suffix-matching the host.
fn cookie_domain_matches(hostname: &str, domain: &str) -> bool {
  if hostname == domain {
    return true;
  }
  if !domain.starts_with('.') {
    return false;
  }
  format!(".{hostname}").ends_with(domain)
}

/// RFC 6265 §5.1.4 path-match (`server/cookieStore.ts::pathMatches`).
fn cookie_path_matches(request_path: &str, cookie_path: &str) -> bool {
  if request_path == cookie_path {
    return true;
  }
  let mut value = request_path.to_string();
  if !value.ends_with('/') {
    value.push('/');
  }
  let mut path = cookie_path.to_string();
  if !path.ends_with('/') {
    path.push('/');
  }
  value.starts_with(&path)
}

/// Whether a context cookie is sent on a request to `url`
/// (`server/cookieStore.ts::Cookie.matches`, plus the expiry prune).
/// `expires`: `None` / negative = session cookie (never expires here);
/// `>= 0` = absolute epoch seconds, pruned when in the past.
fn cookie_matches_url(cookie: &crate::backend::CookieData, url: &reqwest::Url) -> bool {
  let hostname = url.host_str().unwrap_or("");
  if cookie.secure && url.scheme() != "https" && !is_local_hostname(hostname) {
    return false;
  }
  if !cookie_domain_matches(hostname, &cookie.domain) {
    return false;
  }
  if !cookie_path_matches(url.path(), &cookie.path) {
    return false;
  }
  if let Some(expires) = cookie.expires
    && expires >= 0.0
    && expires
      < std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
  {
    return false;
  }
  true
}

/// Parse one `Set-Cookie` header value into a [`CookieData`], porting
/// Playwright's `parseRawCookie` (`server/cookieStore.ts:131`) +
/// `parseCookie` defaults (`server/fetch.ts:889`). Returns `None` for an
/// empty header.
fn parse_raw_set_cookie(header: &str) -> Option<crate::backend::CookieData> {
  let mut pairs = header.split(';').filter(|s| !s.trim().is_empty()).map(|p| {
    p.split_once('=').map_or_else(
      || (p.trim().to_string(), String::new()),
      |(k, v)| (k.trim().to_string(), v.trim().to_string()),
    )
  });
  let (name, value) = pairs.next()?;
  let mut cookie = crate::backend::CookieData {
    name,
    value,
    domain: String::new(),
    path: String::new(),
    secure: false,
    http_only: false,
    expires: None,
    // Unspecified SameSite behaves as Lax (fetch.ts:900 comment); Playwright
    // stores the default explicitly.
    same_site: Some(crate::backend::SameSite::Lax),
    url: None,
  };
  for (attr, attr_value) in pairs {
    match attr.to_ascii_lowercase().as_str() {
      "expires" => {
        // RFC 6265 §5.2.1: unparseable dates are ignored; past dates clamp
        // to the earliest representable time.
        if let Ok(when) = httpdate::parse_http_date(&attr_value) {
          let secs = when
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
          cookie.expires = Some(secs.min(MAX_COOKIE_EXPIRES_SECONDS));
        }
      },
      "max-age" => {
        // RFC 6265 §5.2.2: non-positive delta = earliest representable time.
        if let Ok(delta) = attr_value.parse::<i64>() {
          if delta <= 0 {
            cookie.expires = Some(0.0);
          } else {
            let now = std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .map_or(0.0, |d| d.as_secs_f64());
            // u32 seconds (~136 years) is beyond the RFC clamp anyway;
            // saturating keeps the arithmetic lossless for clippy.
            let delta = f64::from(u32::try_from(delta).unwrap_or(u32::MAX));
            cookie.expires = Some((now + delta).min(MAX_COOKIE_EXPIRES_SECONDS));
          }
        }
      },
      "domain" => {
        let mut domain = attr_value.to_ascii_lowercase();
        // Playwright normalises a dotted-but-not-dot-prefixed domain to its
        // dot-prefixed (subdomain-matching) form.
        if !domain.is_empty() && !domain.starts_with('.') && domain.contains('.') {
          domain.insert(0, '.');
        }
        cookie.domain = domain;
      },
      "path" => cookie.path = attr_value,
      "secure" => cookie.secure = true,
      "httponly" => cookie.http_only = true,
      "samesite" => {
        cookie.same_site = match attr_value.to_ascii_lowercase().as_str() {
          "none" => Some(crate::backend::SameSite::None),
          "strict" => Some(crate::backend::SameSite::Strict),
          "lax" => Some(crate::backend::SameSite::Lax),
          _ => cookie.same_site,
        };
      },
      _ => {},
    }
  }
  Some(cookie)
}

/// Parse every `Set-Cookie` header on a response into browser-ready
/// cookies, applying the RFC 6265 §5.2.3/§5.2.4 domain/path defaults
/// relative to the response URL and dropping cookies whose declared
/// domain does not cover it (`server/fetch.ts::_parseSetCookieHeader`).
fn parse_set_cookie_headers(
  response_url: &reqwest::Url,
  headers: &reqwest::header::HeaderMap,
) -> Vec<crate::backend::CookieData> {
  let hostname = response_url.host_str().unwrap_or("");
  // RFC 6265 §5.1.4 default-path: directory of the request path.
  let path = response_url.path();
  let default_path = {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let segments: Vec<&str> = trimmed.split('/').collect();
    format!("/{}", segments[..segments.len().saturating_sub(1)].join("/"))
  };
  let mut cookies = Vec::new();
  for value in headers.get_all(reqwest::header::SET_COOKIE) {
    let Ok(raw) = value.to_str() else { continue };
    let Some(mut cookie) = parse_raw_set_cookie(raw) else {
      continue;
    };
    if cookie.domain.is_empty() {
      // Host-only cookie: bare response hostname, exact-match semantics.
      cookie.domain = hostname.to_string();
    }
    if !cookie_domain_matches(hostname, &cookie.domain) {
      continue;
    }
    if cookie.path.is_empty() || !cookie.path.starts_with('/') {
      cookie.path.clone_from(&default_path);
    }
    cookies.push(cookie);
  }
  cookies
}

// ── Case-insensitive header-list helpers (Playwright's set/get/removeHeader) ──

fn header_position(headers: &[(String, String)], name: &str) -> Option<usize> {
  headers.iter().position(|(k, _)| k.eq_ignore_ascii_case(name))
}

fn get_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
  header_position(headers, name).map(|i| headers[i].1.as_str())
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
  match header_position(headers, name) {
    Some(i) => headers[i].1 = value,
    None => headers.push((name.to_string(), value)),
  }
}

fn set_header_if_absent(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
  if header_position(headers, name).is_none() {
    headers.push((name.to_string(), value.to_string()));
  }
}

fn remove_header(headers: &mut Vec<(String, String)>, name: &str) {
  headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
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
  /// no per-request client-build cost. The bridged (context-bound) path
  /// stores its variant clients here too, under a `bridged|` key prefix.
  guarded_clients: Arc<Mutex<FxHashMap<String, reqwest::Client>>>,
  /// Browser-context bridge. `Some` for a client minted by
  /// `ContextRef::http_client()` (Playwright's `page.request` /
  /// `context.request`): cookies are read from and written back to the
  /// BROWSER on every hop, defaults come live from the context options,
  /// and redirects are followed manually. `None` = the standalone
  /// client (Playwright's `request.newContext()` analogue) with its own
  /// reqwest jar.
  bridge: Option<Arc<dyn ContextBridge>>,
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

/// Build a hop client for the context-bound path: no cookie store (the
/// browser is the jar) and no automatic redirects (hops are followed
/// manually so each one can bridge cookies both ways). The optional
/// guard only contributes its DNS filter — per-hop URL checks run in
/// the manual loop itself.
fn build_bridged_client(ignore_https_errors: bool, guard: Option<&NetGuard>) -> reqwest::Client {
  let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
  if ignore_https_errors {
    builder = builder.danger_accept_invalid_certs(true);
  }
  if let Some(g) = guard
    && (g.block_metadata || g.block_private)
  {
    builder = builder.dns_resolver(Arc::new(GuardedResolver {
      block_metadata: g.block_metadata,
      block_private: g.block_private,
    }));
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
      bridge: None,
    }
  }

  /// Create a client bound to a browser context (Playwright's
  /// `page.request` / `context.request`). All cookie state lives in the
  /// browser via `bridge`; defaults (`baseURL`, extra headers, UA,
  /// `ignoreHTTPSErrors`) are read live from the context on every
  /// request. The `client` field holds the common-case hop client
  /// (no jar — the browser IS the jar — and no auto-redirects, since
  /// hops are followed manually to bridge cookies per hop).
  #[must_use]
  pub fn context_bound(bridge: Arc<dyn ContextBridge>) -> Self {
    let jar = Arc::new(reqwest::cookie::Jar::default());
    Self {
      client: build_bridged_client(false, None),
      base_url: None,
      extra_headers: Vec::new(),
      default_timeout: Duration::from_secs(30),
      jar,
      ignore_https_errors: false,
      redirect_clients: Arc::new(Mutex::new(FxHashMap::default())),
      guarded_clients: Arc::new(Mutex::new(FxHashMap::default())),
      bridge: Some(bridge),
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
    if let Some(bridge) = self.bridge.clone() {
      return self.send_request_bridged(&bridge, url, opts).await;
    }
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

  /// Hop client for the bridged path: the pre-built common-case client,
  /// or (for `ignoreHTTPSErrors` / an active guard) a cached variant.
  fn bridged_client_for(&self, ignore_https_errors: bool, guard: Option<&NetGuard>) -> reqwest::Client {
    let active_guard = guard.filter(|g| g.is_active());
    if !ignore_https_errors && active_guard.is_none() {
      return self.client.clone();
    }
    let key = format!(
      "bridged|{}|{}",
      u8::from(ignore_https_errors),
      active_guard.map_or_else(String::new, |g| g.cache_key(None))
    );
    let mut cache = self
      .guarded_clients
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
      .entry(key)
      .or_insert_with(|| build_bridged_client(ignore_https_errors, active_guard))
      .clone()
  }

  /// The context-bound request pipeline: Playwright's `_sendRequest`
  /// redirect loop (`server/fetch.ts:340`) with the browser context as
  /// the cookie jar. Per hop: inject the `Cookie` header from the
  /// context (unless the caller set one explicitly — first hop only),
  /// send, write every `Set-Cookie` back into the context, then follow
  /// 301/302/303/307/308 manually with the WHATWG method-rewrite rules.
  async fn send_request_bridged(
    &self,
    bridge: &Arc<dyn ContextBridge>,
    url: &str,
    opts: &RequestOptions,
  ) -> crate::error::Result<(reqwest::Response, String, String)> {
    let defaults = bridge.defaults().await?;

    let method_str = opts.method.as_deref().unwrap_or("GET").to_string();
    let mut method: reqwest::Method = method_str
      .parse()
      .map_err(|_| format!("invalid HTTP method: {method_str}"))?;

    // Playwright resolves against baseURL with full URL-join semantics
    // (`constructURLBasedOnBaseURL` = `new URL(url, baseURL)`).
    let mut request_url = match reqwest::Url::parse(url) {
      Ok(u) => u,
      Err(_) => match &defaults.base_url {
        Some(base) => reqwest::Url::parse(base)
          .and_then(|b| b.join(url))
          .map_err(|e| format!("invalid URL \"{url}\" against baseURL {base:?}: {e}"))?,
        None => {
          return Err(crate::error::FerriError::Backend(format!(
            "invalid URL \"{url}\": no baseURL to resolve against"
          )));
        },
      },
    };
    if let Some(params) = &opts.params {
      let mut qp = request_url.query_pairs_mut();
      for (k, v) in params {
        qp.append_pair(k, v);
      }
    }
    let resolved_url = request_url.to_string();

    let guard = opts.net_guard.as_ref().filter(|g| g.is_active());
    if let Some(g) = guard {
      preflight(&resolved_url, g).map_err(crate::error::FerriError::Backend)?;
    }
    let client = self.bridged_client_for(defaults.ignore_https_errors, guard);

    // Default headers, then context extras, then per-request headers —
    // later writers replace earlier ones (fetch.ts:178).
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(ua) = &defaults.user_agent {
      set_header(&mut headers, "user-agent", ua.clone());
    }
    set_header(&mut headers, "accept", "*/*".to_string());
    for (k, v) in &defaults.extra_http_headers {
      set_header(&mut headers, k, v.clone());
    }
    for (k, v) in &self.extra_headers {
      set_header(&mut headers, k, v.clone());
    }
    if let Some(request_headers) = &opts.headers {
      for (k, v) in request_headers {
        set_header(&mut headers, k, v.clone());
      }
    }
    let explicit_cookie_header = get_header(&headers, "cookie").is_some();

    // Serialized once; re-sent verbatim on 307/308, dropped on a
    // method-rewriting redirect.
    let mut body: Option<Vec<u8>> = if let Some(json) = &opts.json_data {
      set_header_if_absent(&mut headers, "content-type", "application/json");
      Some(serde_json::to_vec(json)?)
    } else if let Some(form) = &opts.form {
      set_header_if_absent(&mut headers, "content-type", "application/x-www-form-urlencoded");
      Some(
        serde_urlencoded::to_string(form)
          .map_err(|e| format!("serialize form data: {e}"))?
          .into_bytes(),
      )
    } else if let Some(data) = &opts.data {
      set_header_if_absent(&mut headers, "content-type", "application/octet-stream");
      Some(data.clone())
    } else {
      None
    };

    // `Some(0)` = don't follow (return the 3xx as-is), `Some(n)` = up to
    // n then error, `None` = Playwright's default of 20.
    let follow_budget: Option<u32> = match opts.max_redirects {
      Some(0) => None,
      Some(n) => Some(n),
      None => Some(20),
    };
    let mut remaining = follow_budget;
    let deadline = tokio::time::Instant::now() + opts.timeout.unwrap_or(self.default_timeout);
    let mut first_hop = true;

    loop {
      if let Some(g) = guard {
        check_url(&request_url, g).map_err(crate::error::FerriError::Backend)?;
      }

      let mut hop_headers = headers.clone();
      if !(first_hop && explicit_cookie_header) {
        remove_header(&mut hop_headers, "cookie");
        let context_cookies = bridge.cookies().await?;
        let value = context_cookies
          .iter()
          .filter(|c| cookie_matches_url(c, &request_url))
          .map(|c| format!("{}={}", c.name, c.value))
          .collect::<Vec<_>>()
          .join("; ");
        if !value.is_empty() {
          set_header(&mut hop_headers, "cookie", value);
        }
      }

      let timeout_left = deadline
        .checked_duration_since(tokio::time::Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| format!("{method_str} {resolved_url} timed out"))?;

      let mut builder = client
        .request(method.clone(), request_url.clone())
        .timeout(timeout_left);
      for (k, v) in &hop_headers {
        builder = builder.header(k, v);
      }
      if let Some(bytes) = &body {
        builder = builder.body(bytes.clone());
      }
      let response = builder
        .send()
        .await
        .map_err(|e| format!("request to {request_url} failed: {e}"))?;

      // Every hop's Set-Cookie goes back into the browser context.
      // Playwright falls back to per-cookie adds when the batch fails
      // (oversized values, or here: a context with no open page).
      let set_cookies = parse_set_cookie_headers(&request_url, response.headers());
      if !set_cookies.is_empty()
        && let Err(batch_err) = bridge.add_cookies(set_cookies.clone()).await
      {
        tracing::warn!("context-bound request: batch addCookies failed ({batch_err}), retrying individually");
        for cookie in set_cookies {
          let name = cookie.name.clone();
          if let Err(e) = bridge.add_cookies(vec![cookie]).await {
            tracing::warn!("context-bound request: dropping Set-Cookie {name:?}: {e}");
          }
        }
      }

      let status = response.status().as_u16();
      let is_redirect = matches!(status, 301 | 302 | 303 | 307 | 308);
      if is_redirect && let Some(budget) = remaining {
        // HTTP-redirect fetch step 4: no Location = return the response.
        let location = response
          .headers()
          .get(reqwest::header::LOCATION)
          .and_then(|v| v.to_str().ok())
          .map(str::to_string);
        let Some(location) = location else {
          return Ok((response, resolved_url, method_str));
        };
        if budget == 0 {
          let max = follow_budget.unwrap_or(0);
          return Err(crate::error::FerriError::Backend(format!(
            "too many redirects (max {max})"
          )));
        }
        let next_url = request_url
          .join(&location)
          .map_err(|_| format!("uri requested responds with an invalid redirect URL: {location}"))?;

        // HTTP-redirect fetch step 13: 301/302 POST and 303 non-GET/HEAD
        // become body-less GETs.
        let rewrite_to_get = ((status == 301 || status == 302) && method == reqwest::Method::POST)
          || (status == 303 && method != reqwest::Method::GET && method != reqwest::Method::HEAD);
        if rewrite_to_get {
          method = reqwest::Method::GET;
          body = None;
          for name in [
            "content-encoding",
            "content-language",
            "content-length",
            "content-location",
            "content-type",
          ] {
            remove_header(&mut headers, name);
          }
        }
        remove_header(&mut headers, "cookie");
        // Credentials are origin-scoped: drop Authorization when the
        // redirect leaves the original origin.
        if next_url.origin() != request_url.origin() {
          remove_header(&mut headers, "authorization");
        }
        request_url = next_url;
        remaining = Some(budget - 1);
        first_hop = false;
        continue;
      }

      return Ok((response, resolved_url, method_str));
    }
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
    let net = ["api.acme.com".to_string(), "*.cdn.com".to_string()];
    assert!(host_allowed("api.acme.com", &net));
    assert!(host_allowed("cdn.com", &net)); // apex
    assert!(host_allowed("a.cdn.com", &net));
    assert!(!host_allowed("evilcdn.com", &net));
    assert!(!host_allowed("acme.com", &net));
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

#[cfg(test)]
mod cookie_parse_tests {
  use super::*;

  fn parse(header: &str, url: &str) -> Option<crate::backend::CookieData> {
    let url = reqwest::Url::parse(url).unwrap();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append(reqwest::header::SET_COOKIE, header.parse().unwrap());
    parse_set_cookie_headers(&url, &headers).into_iter().next()
  }

  #[test]
  fn host_only_defaults_from_response_url() {
    let c = parse("sid=abc", "http://example.com/a/b/c").unwrap();
    assert_eq!(c.name, "sid");
    assert_eq!(c.value, "abc");
    // Host-only: bare hostname, exact-match semantics.
    assert_eq!(c.domain, "example.com");
    // RFC 6265 default-path: directory of the request path.
    assert_eq!(c.path, "/a/b");
    assert_eq!(c.expires, None);
    assert!(matches!(c.same_site, Some(crate::backend::SameSite::Lax)));
  }

  #[test]
  fn declared_domain_gets_dot_prefixed() {
    let c = parse("sid=1; Domain=example.com; Path=/", "http://example.com/").unwrap();
    assert_eq!(c.domain, ".example.com");
    assert_eq!(c.path, "/");
  }

  #[test]
  fn foreign_domain_is_dropped() {
    assert!(parse("sid=1; Domain=evil.com", "http://example.com/").is_none());
  }

  #[test]
  fn attributes_parse() {
    let c = parse(
      "a=b; Secure; HttpOnly; SameSite=Strict; Max-Age=3600; Path=/x",
      "https://example.com/",
    )
    .unwrap();
    assert!(c.secure);
    assert!(c.http_only);
    assert!(matches!(c.same_site, Some(crate::backend::SameSite::Strict)));
    assert_eq!(c.path, "/x");
    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_secs_f64();
    let exp = c.expires.unwrap();
    assert!(exp > now + 3500.0 && exp < now + 3700.0);
  }

  #[test]
  fn non_positive_max_age_expires_immediately() {
    let c = parse("a=b; Max-Age=0; Path=/", "http://example.com/").unwrap();
    assert_eq!(c.expires, Some(0.0));
    let c = parse("a=b; Max-Age=-5; Path=/", "http://example.com/").unwrap();
    assert_eq!(c.expires, Some(0.0));
  }

  #[test]
  fn expires_attribute_parses_http_date() {
    let c = parse(
      "a=b; Expires=Wed, 01 Jan 2031 00:00:00 GMT; Path=/",
      "http://example.com/",
    )
    .unwrap();
    let exp = c.expires.unwrap();
    assert!((1_924_991_940.0..1_924_993_000.0).contains(&exp), "got {exp}");
  }

  #[test]
  fn matcher_domain_and_path() {
    let mk = |domain: &str, path: &str, secure: bool| crate::backend::CookieData {
      name: "n".into(),
      value: "v".into(),
      domain: domain.into(),
      path: path.into(),
      secure,
      http_only: false,
      expires: None,
      same_site: None,
      url: None,
    };
    let url = |u: &str| reqwest::Url::parse(u).unwrap();
    // Host-only: exact host, never subdomains.
    assert!(cookie_matches_url(
      &mk("example.com", "/", false),
      &url("http://example.com/")
    ));
    assert!(!cookie_matches_url(
      &mk("example.com", "/", false),
      &url("http://sub.example.com/")
    ));
    // Dot-prefixed: apex + subdomains.
    assert!(cookie_matches_url(
      &mk(".example.com", "/", false),
      &url("http://sub.example.com/")
    ));
    assert!(cookie_matches_url(
      &mk(".example.com", "/", false),
      &url("http://example.com/")
    ));
    // Path prefix on segment boundary.
    assert!(cookie_matches_url(&mk("e.com", "/a", false), &url("http://e.com/a/b")));
    assert!(!cookie_matches_url(&mk("e.com", "/a", false), &url("http://e.com/ab")));
    // Secure only over https, with the localhost carve-out.
    assert!(!cookie_matches_url(&mk("e.com", "/", true), &url("http://e.com/")));
    assert!(cookie_matches_url(&mk("e.com", "/", true), &url("https://e.com/")));
    assert!(cookie_matches_url(
      &mk("localhost", "/", true),
      &url("http://localhost/")
    ));
  }

  #[test]
  fn expired_cookie_is_not_sent() {
    let c = crate::backend::CookieData {
      name: "n".into(),
      value: "v".into(),
      domain: "e.com".into(),
      path: "/".into(),
      secure: false,
      http_only: false,
      expires: Some(1.0),
      same_site: None,
      url: None,
    };
    assert!(!cookie_matches_url(&c, &reqwest::Url::parse("http://e.com/").unwrap()));
    // Backends report session cookies as -1: never expired.
    let session = crate::backend::CookieData {
      expires: Some(-1.0),
      ..c
    };
    assert!(cookie_matches_url(
      &session,
      &reqwest::Url::parse("http://e.com/").unwrap()
    ));
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
