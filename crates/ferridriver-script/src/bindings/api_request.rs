//! `APIRequestContextJs` + `APIResponseJs`: JS wrappers for HTTP calls from
//! the runner side (separate from the page's own network).

use std::sync::Arc;
use std::time::Duration;

use ferridriver::api_request::{APIRequestContext, APIResponse, RequestOptions};
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use serde::Deserialize;

use crate::bindings::convert::{FerriResultExt, serde_from_js};

/// Shape of per-request options accepted from JS.
///
/// Mirrors `ferridriver::api_request::RequestOptions` but uses
/// `serde::Deserialize` so callers can pass a plain object:
/// `request.post('/api', { json: { x: 1 }, headers: { 'X-A': 'b' } })`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct JsRequestOptions {
  headers: Option<Vec<(String, String)>>,
  data: Option<Vec<u8>>,
  json: Option<serde_json::Value>,
  form: Option<Vec<(String, String)>>,
  params: Option<Vec<(String, String)>>,
  /// Per-request timeout in milliseconds.
  timeout_ms: Option<u64>,
  fail_on_status_code: Option<bool>,
  max_redirects: Option<u32>,
}

impl JsRequestOptions {
  fn into_core(self) -> RequestOptions {
    RequestOptions {
      method: None,
      headers: self.headers,
      data: self.data,
      json_data: self.json,
      form: self.form,
      params: self.params,
      timeout: self.timeout_ms.map(Duration::from_millis),
      fail_on_status_code: self.fail_on_status_code,
      max_redirects: self.max_redirects,
    }
  }
}

fn parse_options<'js>(ctx: &Ctx<'js>, value: Opt<Value<'js>>) -> rquickjs::Result<Option<RequestOptions>> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let parsed: JsRequestOptions = serde_from_js(ctx, v)?;
      Ok(Some(parsed.into_core()))
    },
    _ => Ok(None),
  }
}

// ── APIRequestContextJs ──────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "APIRequestContext")]
pub struct APIRequestContextJs {
  #[qjs(skip_trace)]
  inner: Arc<APIRequestContext>,
  /// Host allow-list (plugin `allow.net` capability). Empty =
  /// unrestricted. Non-empty = default-deny: every request URL's host
  /// must match an entry (exact, or `*.suffix` which also matches the
  /// bare apex) or the call throws before any network I/O. Enforced
  /// natively in Rust here — there is no JS proxy/shim.
  #[qjs(skip_trace)]
  net: Arc<[String]>,
}

impl APIRequestContextJs {
  #[must_use]
  pub fn new(inner: Arc<APIRequestContext>) -> Self {
    Self {
      inner,
      net: Arc::from([]),
    }
  }

  /// Same underlying context, restricted to `net` hosts. Used to build
  /// the per-tool `request` a plugin handler receives when its manifest
  /// declares `allow.net`.
  #[must_use]
  pub fn with_net(inner: Arc<APIRequestContext>, net: Arc<[String]>) -> Self {
    Self { inner, net }
  }

  /// The shared underlying context — lets the plugin dispatch wrap the
  /// session's `request` with a net allow-list without re-creating it.
  #[must_use]
  pub fn inner_arc(&self) -> Arc<APIRequestContext> {
    self.inner.clone()
  }

  /// Default-deny host check. `Ok(())` when `net` is empty (unrestricted)
  /// or the URL's host matches an allow-list entry; otherwise a JS-thrown
  /// error naming the host. No network I/O happens on rejection.
  fn guard(&self, url: &str) -> rquickjs::Result<()> {
    if self.net.is_empty() {
      return Ok(());
    }
    let host = host_of(url).ok_or_else(|| {
      rquickjs::Error::new_from_js_message(
        "request",
        "Error",
        format!("request to invalid/relative URL \"{url}\" is not permitted by allow.net"),
      )
    })?;
    if host_allowed(&host, &self.net) {
      Ok(())
    } else {
      Err(rquickjs::Error::new_from_js_message(
        "request",
        "Error",
        format!("request host \"{host}\" is not in allow.net {:?}", &*self.net),
      ))
    }
  }
}

/// Extract the lowercased host (no port, no userinfo) from an absolute
/// URL. Returns `None` for relative/invalid input — the caller treats
/// that as a denial when a net allow-list is active.
fn host_of(url: &str) -> Option<String> {
  let after_scheme = url.split_once("://")?.1;
  let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or(after_scheme);
  let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
  let host = if let Some(stripped) = host_port.strip_prefix('[') {
    // IPv6 literal: take up to the closing bracket.
    stripped.split(']').next().unwrap_or(stripped)
  } else {
    host_port.split(':').next().unwrap_or(host_port)
  };
  (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Match a host against one allow-list: exact, or a leading-wildcard
/// suffix (`*.box.com` also matches the bare apex `box.com`).
fn host_allowed(host: &str, net: &[String]) -> bool {
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

#[rquickjs::methods]
impl APIRequestContextJs {
  #[qjs(rename = "get")]
  pub async fn get<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<APIResponseJs> {
    self.guard(&url)?;
    let opts = parse_options(&ctx, options)?;
    let resp = self.inner.get(&url, opts).await.into_js()?;
    Ok(APIResponseJs::new(resp))
  }

  #[qjs(rename = "post")]
  pub async fn post<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<APIResponseJs> {
    self.guard(&url)?;
    let opts = parse_options(&ctx, options)?;
    let resp = self.inner.post(&url, opts).await.into_js()?;
    Ok(APIResponseJs::new(resp))
  }

  #[qjs(rename = "put")]
  pub async fn put<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<APIResponseJs> {
    self.guard(&url)?;
    let opts = parse_options(&ctx, options)?;
    let resp = self.inner.put(&url, opts).await.into_js()?;
    Ok(APIResponseJs::new(resp))
  }

  #[qjs(rename = "delete")]
  pub async fn delete<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<APIResponseJs> {
    self.guard(&url)?;
    let opts = parse_options(&ctx, options)?;
    let resp = self.inner.delete(&url, opts).await.into_js()?;
    Ok(APIResponseJs::new(resp))
  }

  #[qjs(rename = "patch")]
  pub async fn patch<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<APIResponseJs> {
    self.guard(&url)?;
    let opts = parse_options(&ctx, options)?;
    let resp = self.inner.patch(&url, opts).await.into_js()?;
    Ok(APIResponseJs::new(resp))
  }

  #[qjs(rename = "head")]
  pub async fn head<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<APIResponseJs> {
    self.guard(&url)?;
    let opts = parse_options(&ctx, options)?;
    let resp = self.inner.head(&url, opts).await.into_js()?;
    Ok(APIResponseJs::new(resp))
  }

  /// Generic fetch — `options` may include `method` via `headers` only; this
  /// mirrors `RequestOptions` (no request overload for now — see the
  /// `PLAYWRIGHT_COMPAT.md` gap for `APIRequestContext.fetch(Request, ...)`).
  #[qjs(rename = "fetch")]
  pub async fn fetch<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<APIResponseJs> {
    self.guard(&url)?;
    let opts = parse_options(&ctx, options)?;
    let resp = self.inner.fetch(&url, opts).await.into_js()?;
    Ok(APIResponseJs::new(resp))
  }
}

// ── APIResponseJs ────────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "APIResponse")]
pub struct APIResponseJs {
  #[qjs(skip_trace)]
  inner: APIResponse,
}

impl APIResponseJs {
  #[must_use]
  pub fn new(inner: APIResponse) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl APIResponseJs {
  #[qjs(rename = "status")]
  pub fn status(&self) -> i32 {
    i32::from(self.inner.status())
  }

  #[qjs(rename = "statusText")]
  pub fn status_text(&self) -> String {
    self.inner.status_text().to_string()
  }

  #[qjs(rename = "url")]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  #[qjs(rename = "ok")]
  pub fn ok(&self) -> bool {
    self.inner.ok()
  }

  /// All response headers as an array of `{name, value}` tuples (Playwright's
  /// `headersArray` shape).
  #[qjs(rename = "headersArray")]
  pub fn headers_array<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let h = self.inner.headers();
    let pairs: Vec<(&str, &str)> = h.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
    crate::bindings::convert::name_value_array_to_js(&ctx, &pairs)
  }

  /// Value of a single header, or `null` if absent.
  #[qjs(rename = "header")]
  pub fn header(&self, name: String) -> Option<String> {
    self.inner.header(&name).map(str::to_string)
  }

  /// Response body as UTF-8 text.
  #[qjs(rename = "text")]
  pub fn text(&self) -> rquickjs::Result<String> {
    self.inner.text().into_js()
  }

  /// Response body parsed as JSON.
  #[qjs(rename = "json")]
  pub fn json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    // Parse the raw body straight into a JS value with QuickJS's C JSON
    // parser — no serde_json::Value middle allocation. `json_parse`
    // does not touch the JS `JSON` global, so a reassigned
    // `globalThis.JSON` cannot affect it.
    let text = self.inner.text().into_js()?;
    ctx.json_parse(text)
  }
}
