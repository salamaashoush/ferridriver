//! `BiDi` network route action execution and `continue({ url })` re-fetch.

use super::super::{CookieData, Engine, FerriError, FxHashMap, Result, json};

/// Execute a route action via `BiDi` network commands.
pub(in crate::backend::bidi::page) async fn execute_bidi_route_action(
  transport: &crate::backend::bidi::transport::BidiTransport,
  request_id: &str,
  action: crate::route::RouteAction,
  orig_method: &str,
  orig_headers: &FxHashMap<String, String>,
  network: &NetworkSettings,
) {
  match action {
    crate::route::RouteAction::Fulfill(resp) => {
      let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp.body);
      let hdrs: Vec<serde_json::Value> = resp
        .effective_headers()
        .into_iter()
        .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
        .collect();
      let _ = transport
        .send_command(
          "network.provideResponse",
          json!({
            "request": request_id,
            "statusCode": resp.status,
            "reasonPhrase": crate::route::status_text(resp.status),
            "headers": hdrs,
            "body": {"type": "base64", "value": body_b64},
          }),
        )
        .await;
    },
    crate::route::RouteAction::Continue(overrides) | crate::route::RouteAction::Fallback(overrides) => {
      if let Some(target_url) = &overrides.url {
        // Continue with a URL override. Firefox/BiDi `network.continueRequest`
        // accepts a `url` field but then aborts the request instead of
        // honoring it (verified against Firefox; Playwright sends it
        // best-effort and it is silently dropped). Implement the rewrite
        // faithfully: issue the request to the new URL ourselves — applying
        // the method/header/body overrides on top of the original request's
        // method/headers (so cookies and auth carry over) — and fulfill the
        // ORIGINAL request with that response. The page sees its own request
        // resolve to the new URL's content: `response.url` stays the original
        // and `response.redirected` is false, exactly like a native continue
        // with a rewritten URL.
        let method = overrides.method.as_deref().unwrap_or(orig_method);
        // Original headers, then apply the override headers on top.
        let mut merged: Vec<(String, String)> = orig_headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(over) = &overrides.headers {
          merged.retain(|(k, _)| !over.iter().any(|(ok, _)| ok.eq_ignore_ascii_case(k)));
          merged.extend(over.iter().cloned());
        }
        bidi_continue_via_fetch(
          transport,
          request_id,
          RouteRefetch {
            url: target_url,
            method,
            headers: &merged,
            body: overrides.post_data.as_deref(),
            network,
          },
        )
        .await;
      } else {
        let hdrs: Option<Vec<serde_json::Value>> = overrides.headers.as_ref().map(|headers| {
          headers
            .iter()
            .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
            .collect()
        });
        let body = overrides.post_data.as_ref().map(|post_data| {
          let encoded = base64::engine::general_purpose::STANDARD.encode(post_data);
          json!({"type": "base64", "value": encoded})
        });
        let _ = transport
          .send(&ferridriver_bidi_proto::network::ContinueRequest {
            request: request_id,
            body,
            headers: hdrs.as_deref(),
            method: overrides.method.as_deref(),
            url: None,
          })
          .await;
      }
    },
    crate::route::RouteAction::Abort(_reason) => {
      let _ = transport
        .send(&ferridriver_bidi_proto::network::FailRequest { request: request_id })
        .await;
    },
    // Chain-internal marker; never emitted as a terminal action. Fail
    // open like an unhandled route if it ever leaks.
    crate::route::RouteAction::PredicateRejected => {
      let _ = transport
        .send(&ferridriver_bidi_proto::network::ContinueRequest {
          request: request_id,
          body: None,
          headers: None,
          method: None,
          url: None,
        })
        .await;
    },
  }
}

/// A route's `continue({ url })` as this backend re-issues it.
struct RouteRefetch<'a> {
  url: &'a str,
  method: &'a str,
  headers: &'a [(String, String)],
  body: Option<&'a [u8]>,
  network: &'a NetworkSettings,
}

/// Continue an intercepted request with a `url` override on `BiDi` by issuing
/// the request to the new URL ourselves and fulfilling the ORIGINAL request
/// with the result — Firefox's `network.continueRequest` `url` field aborts
/// rather than rewriting. The page's request resolves to the new URL's
/// content (`response.url` stays original, not a redirect). On any failure
/// the original request is continued unmodified so the page never stalls.
async fn bidi_continue_via_fetch(
  transport: &crate::backend::bidi::transport::BidiTransport,
  request_id: &str,
  refetch: RouteRefetch<'_>,
) {
  match fetch_for_route(&refetch).await {
    Ok((status, resp_headers, resp_body)) => {
      let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp_body);
      let hdrs: Vec<serde_json::Value> = resp_headers
        .iter()
        .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
        .collect();
      let _ = transport
        .send(&ferridriver_bidi_proto::network::ProvideResponse {
          request: request_id,
          status_code: Some(status),
          reason_phrase: Some(crate::route::status_text(i32::from(status))),
          headers: Some(&hdrs),
          body: Some(json!({"type": "base64", "value": body_b64})),
        })
        .await;
    },
    Err(e) => {
      tracing::warn!("bidi route url-override fetch failed ({e}); continuing request unmodified");
      let _ = transport
        .send(&ferridriver_bidi_proto::network::ContinueRequest {
          request: request_id,
          body: None,
          headers: None,
          method: None,
          url: None,
        })
        .await;
    },
  }
}

/// Issue the HTTP request for a route `url` override, through the context's
/// proxy and with its TLS posture, as the browser would have sent it.
/// Hop-by-hop / length headers are dropped (the client sets them on the
/// request; the browser recomputes them when the fulfilled body is
/// delivered).
async fn fetch_for_route(refetch: &RouteRefetch<'_>) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
  let headers: Vec<(String, String)> = refetch
    .headers
    .iter()
    .filter(|(k, _)| {
      !matches!(
        k.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "connection" | "accept-encoding"
      )
    })
    .cloned()
    .collect();
  let client = crate::http_client::HttpClient::new(crate::http_client::HttpClientOptions {
    ignore_https_errors: refetch.network.ignore_https_errors,
    proxy: refetch.network.proxy.clone(),
    ..Default::default()
  });
  let response = client
    .fetch(refetch.url)
    .options(crate::http_client::RequestOptions {
      method: Some(refetch.method.to_string()),
      headers: Some(headers),
      data: refetch.body.map(<[u8]>::to_vec),
      fail_on_status_code: Some(false),
      ..Default::default()
    })
    .await
    .map_err(|e| FerriError::protocol("route url override", format!("request to {} failed: {e}", refetch.url)))?;

  let status = response.status();
  let resp_headers: Vec<(String, String)> = response
    .headers()
    .iter()
    .filter(|(k, _)| {
      !matches!(
        k.to_ascii_lowercase().as_str(),
        "content-length" | "transfer-encoding" | "connection" | "content-encoding"
      )
    })
    .cloned()
    .collect();
  Ok((status, resp_headers, response.body()?.to_vec()))
}

/// The proxy and TLS posture a page's context was made with.
#[derive(Debug, Clone, Default)]
pub(crate) struct NetworkSettings {
  pub(crate) proxy: Option<crate::options::ProxyConfig>,
  pub(crate) ignore_https_errors: bool,
}

/// One attempt per request and per source: a proxy's challenge and then the
/// site's for the same request are two attempts, as in Playwright's
/// `authAttemptKey`.
pub(in crate::backend::bidi::page) fn auth_attempt_key(request: &str, from_proxy: bool) -> String {
  format!("{request}:{}", if from_proxy { "proxy" } else { "server" })
}

pub(in crate::backend::bidi::page) fn forget_auth_attempts(
  attempted: &mut rustc_hash::FxHashSet<String>,
  request: &str,
) {
  attempted.remove(&auth_attempt_key(request, true));
  attempted.remove(&auth_attempt_key(request, false));
}

/// Parse a `BiDi` network cookie into our `CookieData`.
pub(in crate::backend::bidi::page) fn parse_bidi_cookie(c: &serde_json::Value) -> CookieData {
  let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let value = c
    .get("value")
    .and_then(|v| {
      // BiDi cookies have {type: "string", value: "..."} format
      v.get("value").and_then(|inner| inner.as_str()).or_else(|| v.as_str())
    })
    .unwrap_or("")
    .to_string();
  let domain = c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let path = c.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string();
  let secure = c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false);
  let http_only = c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false);
  let expires = c.get("expiry").and_then(serde_json::Value::as_f64);
  let same_site = c.get("sameSite").and_then(|v| v.as_str()).and_then(|s| match s {
    "strict" => Some(crate::backend::SameSite::Strict),
    "lax" => Some(crate::backend::SameSite::Lax),
    "none" => Some(crate::backend::SameSite::None),
    _ => None,
  });

  CookieData {
    name,
    value,
    domain,
    path,
    secure,
    http_only,
    expires,
    same_site,
    url: None,
  }
}

/// The `prefers-color-scheme` value for `emulation.setMediaFeaturesOverride`:
/// the scheme, or `null` for the browser's own appearance (which, unlike
/// leaving the feature out, also sets aside a user context's override).
/// `None` when nothing is asked of it.
pub(in crate::backend::bidi::page) fn color_scheme_feature(
  scheme: &crate::options::MediaOverride,
) -> Result<Option<serde_json::Value>> {
  use crate::options::MediaOverride;
  match scheme {
    MediaOverride::Unchanged => Ok(None),
    MediaOverride::Disabled => Ok(Some(serde_json::Value::Null)),
    MediaOverride::Set(value) if value == "light" || value == "dark" => Ok(Some(json!(value))),
    MediaOverride::Set(value) => Err(FerriError::unsupported(format!(
      "BiDi/Firefox emulates `colorScheme` as light or dark only, not `{value}`"
    ))),
  }
}

/// The one `browser.setDownloadBehavior` payload both senders use.
///
/// Firefox takes a destination folder only for the allowed case; a
/// `denied` behaviour carrying one is rejected.
pub(in crate::backend::bidi::page) fn download_behavior_params(
  accept_downloads: &std::sync::atomic::AtomicBool,
  downloads_subdir: &std::path::Path,
  user_context: Option<&str>,
) -> serde_json::Value {
  let behavior = if accept_downloads.load(std::sync::atomic::Ordering::Relaxed) {
    serde_json::json!({
      "type": "allowed",
      "destinationFolder": downloads_subdir.to_string_lossy(),
    })
  } else {
    serde_json::json!({ "type": "denied" })
  };
  let mut params = serde_json::json!({ "downloadBehavior": behavior });
  if let Some(uc) = user_context {
    params["userContexts"] = serde_json::json!([uc]);
  }
  params
}
