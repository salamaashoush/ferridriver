//! The one send engine.
//!
//! A single manual-redirect loop over `reqwest` handles every request:
//! `fetch` global, Playwright `request`, standalone or context-bound.
//! reqwest is only the per-hop transport (`redirect::Policy::none()`);
//! redirect following, cookie bridging, retries, the net-guard per-hop
//! check, and the timeout budget all live here. Cookies come from the
//! reqwest jar (standalone) or the browser context (bridged) — the only
//! difference between the two.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustc_hash::FxHashMap;

use super::bridge::ContextBridge;
use super::cookie::{cookie_matches_url, parse_set_cookie_headers};
use super::error::FetchError;
use super::headers::Headers;
use super::model::{Credentials, RedirectMode, RemoteAddr, Request, Response, ResponseType};
use super::net_guard::{GuardedResolver, NetGuard, check_url, preflight};

/// reqwest client-cache key: `(ignore_https, dns-guard filter, attach jar)`.
/// All clients share the pool's redirect policy (`none`); `attach jar`
/// is `false` for a `credentials: omit` request so no cookies ride it.
type ClientKey = (bool, Option<(bool, bool)>, bool);

/// A cache of `reqwest::Client`s that differ only in TLS posture and the
/// DNS-layer guard filter. Every client follows no redirects (the loop
/// does) and shares the pool's cookie jar, if it has one.
#[derive(Clone)]
pub(crate) struct ClientPool {
  base: reqwest::Client,
  /// `Some` for a standalone client (reqwest owns the jar); `None` for a
  /// context-bound client (the browser is the jar).
  jar: Option<Arc<reqwest::cookie::Jar>>,
  default_ignore_https: bool,
  variants: Arc<Mutex<FxHashMap<ClientKey, reqwest::Client>>>,
}

impl ClientPool {
  /// A standalone pool with its own reqwest cookie jar.
  pub(crate) fn standalone(ignore_https: bool) -> Self {
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let base = build_client(Some(&jar), ignore_https, None);
    Self {
      base,
      jar: Some(jar),
      default_ignore_https: ignore_https,
      variants: Arc::new(Mutex::new(FxHashMap::default())),
    }
  }

  /// A context-bound pool: no jar (cookies live in the browser).
  pub(crate) fn bridged() -> Self {
    Self {
      base: build_client(None, false, None),
      jar: None,
      default_ignore_https: false,
      variants: Arc::new(Mutex::new(FxHashMap::default())),
    }
  }

  fn client(&self, ignore_https: bool, guard: Option<&NetGuard>, use_jar: bool) -> reqwest::Client {
    let dns = guard.and_then(NetGuard::dns_filter);
    // Attach the jar only when the request wants credentials AND the pool
    // owns one (bridged pools have none — cookies ride the browser).
    let attach_jar = use_jar && self.jar.is_some();
    if ignore_https == self.default_ignore_https && dns.is_none() && attach_jar == self.jar.is_some() {
      return self.base.clone();
    }
    let jar = attach_jar.then(|| self.jar.clone()).flatten();
    let mut cache = self.variants.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
      .entry((ignore_https, dns, attach_jar))
      .or_insert_with(|| build_client(jar.as_ref(), ignore_https, dns))
      .clone()
  }
}

/// Build a no-redirect reqwest client. `jar` is attached only for the
/// standalone path; `dns` installs the SSRF address filter.
fn build_client(
  jar: Option<&Arc<reqwest::cookie::Jar>>,
  ignore_https: bool,
  dns: Option<(bool, bool)>,
) -> reqwest::Client {
  let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
  if let Some(jar) = jar {
    builder = builder.cookie_provider(jar.clone());
  }
  if ignore_https {
    builder = builder.danger_accept_invalid_certs(true);
  }
  if let Some((block_metadata, block_private)) = dns {
    builder = builder.dns_resolver(Arc::new(GuardedResolver {
      block_metadata,
      block_private,
    }));
  }
  builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// Whether an error message denotes a connection reset (the only class
/// Playwright retries — `maxRetries`, ECONNRESET).
fn is_reset_message(message: &str) -> bool {
  let m = message.to_ascii_lowercase();
  m.contains("connection reset") || m.contains("econnreset")
}

/// Whether a transport error is a connection reset. reqwest does not
/// surface the errno directly, so the source chain is inspected for a
/// `ConnectionReset` io error or a reset message.
fn is_connection_reset(err: &reqwest::Error) -> bool {
  let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
  while let Some(e) = source {
    if let Some(io) = e.downcast_ref::<std::io::Error>()
      && io.kind() == std::io::ErrorKind::ConnectionReset
    {
      return true;
    }
    if is_reset_message(&e.to_string()) {
      return true;
    }
    source = e.source();
  }
  false
}

/// Exponential backoff for retry attempt `n` (1-based): 250ms, 500ms,
/// 1s, … — mirrors Playwright's `_sendRequestWithRetries` schedule.
fn retry_backoff(attempt: u32) -> Duration {
  Duration::from_millis(250u64.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1))))
}

/// The redirect budget for a request: `None` = never follow (`manual` /
/// `error`, or `follow` with an explicit cap of 0); `Some(n)` = follow up
/// to `n`; `follow` with no cap defaults to 20 (Playwright's default).
fn follow_budget(redirect: RedirectMode, max_redirects: Option<u32>) -> Option<u32> {
  match redirect {
    RedirectMode::Manual | RedirectMode::Error => None,
    RedirectMode::Follow => match max_redirects {
      Some(0) => None,
      Some(n) => Some(n),
      None => Some(20),
    },
  }
}

/// The `Cookie` header for one hop.
///
/// `credentials: omit` sends none. The context-bound path assembles it
/// from the browser jar, except on the first hop when the caller set one
/// explicitly. The standalone path leaves the header alone (the pool's
/// reqwest jar fills it in).
async fn hop_headers(
  bridge: Option<&Arc<dyn ContextBridge>>,
  credentials: Credentials,
  headers: &Headers,
  request_url: &reqwest::Url,
  keep_explicit_cookie: bool,
) -> Result<Headers, FetchError> {
  let mut hop = headers.clone();
  if credentials == Credentials::Omit {
    hop.remove("cookie");
    return Ok(hop);
  }
  let Some(bridge) = bridge else { return Ok(hop) };
  if keep_explicit_cookie {
    return Ok(hop);
  }
  hop.remove("cookie");
  let context_cookies = bridge.cookies().await.map_err(|e| FetchError::Network(e.to_string()))?;
  let value = context_cookies
    .iter()
    .filter(|c| cookie_matches_url(c, request_url))
    .map(|c| format!("{}={}", c.name, c.value))
    .collect::<Vec<_>>()
    .join("; ");
  if !value.is_empty() {
    hop.set("cookie", value);
  }
  Ok(hop)
}

/// One hop over the wire, retrying a connection reset up to `max_retries`.
#[allow(clippy::too_many_arguments)]
async fn send_hop(
  client: &reqwest::Client,
  method: &reqwest::Method,
  request_url: &reqwest::Url,
  headers: &Headers,
  body: Option<&bytes::Bytes>,
  deadline: tokio::time::Instant,
  max_retries: u32,
  timeout_message: &str,
) -> Result<reqwest::Response, FetchError> {
  let mut attempt = 0u32;
  loop {
    let timeout_left = deadline
      .checked_duration_since(tokio::time::Instant::now())
      .filter(|d| !d.is_zero())
      .ok_or_else(|| FetchError::Timeout(timeout_message.to_string()))?;
    let mut builder = client
      .request(method.clone(), request_url.clone())
      .timeout(timeout_left);
    for (k, v) in headers.iter() {
      builder = builder.header(k, v);
    }
    if let Some(bytes) = body {
      builder = builder.body(bytes.clone());
    }
    match builder.send().await {
      Ok(response) => return Ok(response),
      Err(e) if attempt < max_retries && is_connection_reset(&e) => {
        attempt += 1;
        tokio::time::sleep(retry_backoff(attempt)).await;
      },
      Err(e) => return Err(FetchError::Network(format!("request to {request_url} failed: {e}"))),
    }
  }
}

/// Write a hop's `Set-Cookie`s back into the browser context.
///
/// Playwright falls back to per-cookie adds when the batch fails
/// (oversized values, or here: a context with no open page).
async fn persist_set_cookies(
  bridge: &Arc<dyn ContextBridge>,
  request_url: &reqwest::Url,
  response: &reqwest::Response,
) {
  let set_cookies = parse_set_cookie_headers(request_url, response.headers());
  if set_cookies.is_empty() {
    return;
  }
  let Err(batch_err) = bridge.add_cookies(set_cookies.clone()).await else {
    return;
  };
  tracing::warn!("context-bound request: batch addCookies failed ({batch_err}), retrying individually");
  for cookie in set_cookies {
    let name = cookie.name.clone();
    if let Err(e) = bridge.add_cookies(vec![cookie]).await {
      tracing::warn!("context-bound request: dropping Set-Cookie {name:?}: {e}");
    }
  }
}

/// The absolute URL a 3xx points at, or `None` when it carries no
/// `Location` (HTTP-redirect fetch step 4: return the response as-is).
fn redirect_target(
  request_url: &reqwest::Url,
  response: &reqwest::Response,
) -> Result<Option<reqwest::Url>, FetchError> {
  let Some(location) = response
    .headers()
    .get(reqwest::header::LOCATION)
    .and_then(|v| v.to_str().ok())
  else {
    return Ok(None);
  };
  request_url.join(location).map(Some).map_err(|_| {
    FetchError::InvalidUrl(format!(
      "uri requested responds with an invalid redirect URL: {location}"
    ))
  })
}

/// Apply HTTP-redirect fetch's request rewrites for one hop: 301/302
/// POST and 303 non-GET/HEAD become body-less GETs, the `Cookie` header
/// is always re-derived, and `Authorization` is dropped on a
/// cross-origin hop (credentials are origin-scoped).
fn rewrite_for_redirect(
  status: u16,
  request_url: &reqwest::Url,
  next_url: &reqwest::Url,
  method: &mut reqwest::Method,
  body: &mut Option<bytes::Bytes>,
  headers: &mut Headers,
) {
  let rewrite_to_get = ((status == 301 || status == 302) && *method == reqwest::Method::POST)
    || (status == 303 && *method != reqwest::Method::GET && *method != reqwest::Method::HEAD);
  if rewrite_to_get {
    *method = reqwest::Method::GET;
    *body = None;
    for name in [
      "content-encoding",
      "content-language",
      "content-length",
      "content-location",
      "content-type",
    ] {
      headers.remove(name);
    }
  }
  headers.remove("cookie");
  if next_url.origin() != request_url.origin() {
    headers.remove("authorization");
  }
}

/// Send a fully-resolved [`Request`] and return the [`Response`].
///
/// `bridge` present ⇒ the context-bound path (cookies read from / written
/// back to the browser per hop). Absent ⇒ the standalone path (the pool's
/// reqwest jar carries cookies).
///
/// # Errors
///
/// Returns a [`FetchError`] for a transport failure, an SSRF-guard
/// denial, a redirect-budget overrun, a `redirect: error` 3xx, or a
/// timeout.
pub(crate) async fn send(
  pool: &ClientPool,
  bridge: Option<&Arc<dyn ContextBridge>>,
  req: Request,
) -> Result<Response, FetchError> {
  let Request {
    mut method,
    url,
    mut headers,
    body,
    redirect,
    credentials,
    max_redirects,
    max_retries,
    timeout,
    ignore_https_errors,
    net_guard,
  } = req;

  let method_str = method.to_string();
  let resolved_url = url.to_string();
  let mut body = body.into_request_bytes();

  let guard = net_guard.as_ref().filter(|g| g.is_active());
  if let Some(g) = guard {
    preflight(&resolved_url, g).map_err(FetchError::Blocked)?;
  }
  // `credentials: omit` rides a jar-less client so no stored cookie is
  // sent and no `Set-Cookie` is stored.
  let client = pool.client(ignore_https_errors, guard, credentials != Credentials::Omit);

  let explicit_cookie_header = headers.contains("cookie");
  let mut remaining = follow_budget(redirect, max_redirects);
  let deadline = tokio::time::Instant::now() + timeout;
  let mut request_url = url;
  let mut first_hop = true;
  let mut hops_followed = 0u32;

  let response = 'redirects: loop {
    if let Some(g) = guard {
      check_url(&request_url, g).map_err(FetchError::Blocked)?;
    }

    let hop = hop_headers(
      bridge,
      credentials,
      &headers,
      &request_url,
      first_hop && explicit_cookie_header,
    )
    .await?;

    let response = send_hop(
      &client,
      &method,
      &request_url,
      &hop,
      body.as_ref(),
      deadline,
      max_retries,
      &format!("{method_str} {resolved_url} timed out"),
    )
    .await?;

    if let Some(bridge) = bridge {
      persist_set_cookies(bridge, &request_url, &response).await;
    }

    let status = response.status().as_u16();
    if matches!(status, 301 | 302 | 303 | 307 | 308)
      && let Some(budget) = remaining
    {
      if budget == 0 {
        return Err(FetchError::TooManyRedirects(
          follow_budget(redirect, max_redirects).unwrap_or(0),
        ));
      }
      // HTTP-redirect fetch step 4: no Location = return the response.
      let Some(next_url) = redirect_target(&request_url, &response)? else {
        break 'redirects response;
      };
      rewrite_for_redirect(status, &request_url, &next_url, &mut method, &mut body, &mut headers);
      request_url = next_url;
      remaining = Some(budget - 1);
      first_hop = false;
      hops_followed += 1;
      continue;
    }

    break 'redirects response;
  };

  if redirect == RedirectMode::Error && response.status().is_redirection() {
    return Err(FetchError::RedirectRefused(format!(
      "{method_str} {resolved_url}: unexpected redirect (redirect: \"error\")"
    )));
  }
  Ok(into_response(response, &request_url, redirect, hops_followed))
}

/// Turn the final reqwest response into the typed [`Response`], applying
/// the WHATWG response-type filter (`manual` + 3xx = opaque redirect).
fn into_response(
  response: reqwest::Response,
  request_url: &reqwest::Url,
  redirect: RedirectMode,
  hops_followed: u32,
) -> Response {
  let status = response.status().as_u16();
  let status_text = response.status().canonical_reason().unwrap_or("Unknown").to_string();
  let server_addr = response.remote_addr().map(|addr| RemoteAddr {
    ip_address: addr.ip().to_string(),
    port: addr.port(),
  });
  let headers: Headers = response
    .headers()
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
    .collect::<Vec<_>>()
    .into();
  let unfollowed_redirect = redirect == RedirectMode::Manual && response.status().is_redirection();
  Response {
    status,
    status_text,
    url: request_url.to_string(),
    headers,
    body: super::body::Body::from_response(response),
    redirected: hops_followed > 0,
    unfollowed_redirect,
    server_addr,
    type_: if unfollowed_redirect {
      ResponseType::OpaqueRedirect
    } else {
      ResponseType::Basic
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reset_message_detection() {
    assert!(is_reset_message(
      "error sending request: Connection reset by peer (os error 54)"
    ));
    assert!(is_reset_message("ECONNRESET"));
    assert!(!is_reset_message("connection closed before message completed"));
    assert!(!is_reset_message("404 Not Found"));
  }

  #[test]
  fn retry_backoff_is_exponential() {
    assert_eq!(retry_backoff(1), Duration::from_millis(250));
    assert_eq!(retry_backoff(2), Duration::from_millis(500));
    assert_eq!(retry_backoff(3), Duration::from_millis(1000));
  }

  #[test]
  fn follow_budget_maps_modes() {
    assert_eq!(follow_budget(RedirectMode::Follow, None), Some(20));
    assert_eq!(follow_budget(RedirectMode::Follow, Some(0)), None);
    assert_eq!(follow_budget(RedirectMode::Follow, Some(3)), Some(3));
    assert_eq!(follow_budget(RedirectMode::Manual, None), None);
    assert_eq!(follow_budget(RedirectMode::Error, Some(5)), None);
  }
}
