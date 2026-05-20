//! `HttpClientJs` + `HttpResponseJs`: JS wrappers for HTTP calls from
//! the runner side (separate from the page's own network).

use std::sync::Arc;
use std::time::Duration;

use ferridriver::http_client::{HttpClient, HttpResponse, NetGuard, RequestOptions};
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use serde::Deserialize;

use crate::bindings::convert::{FerriResultExt, serde_from_js};

/// Shape of per-request options accepted from JS.
///
/// Mirrors `ferridriver::http_client::RequestOptions` but uses
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
      // Set by `with_guard` after parsing — never from JS input.
      net_guard: None,
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

// ── HttpClientJs ──────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "HttpClient")]
pub struct HttpClientJs {
  #[qjs(skip_trace)]
  inner: Arc<HttpClient>,
  /// Host allow-list (plugin `allow.net` capability). Empty =
  /// unrestricted. Non-empty = default-deny: every request URL's host
  /// must match an entry (exact, or `*.suffix` which also matches the
  /// bare apex) or the call throws before any network I/O. Enforced
  /// natively in Rust here — there is no JS proxy/shim.
  #[qjs(skip_trace)]
  net: Arc<[String]>,
}

impl HttpClientJs {
  #[must_use]
  pub fn new(inner: Arc<HttpClient>) -> Self {
    Self {
      inner,
      net: Arc::from([]),
    }
  }

  /// Same underlying context, restricted to `net` hosts. Used to build
  /// the per-tool `request` a plugin handler receives when its manifest
  /// declares `allow.net`.
  #[must_use]
  pub fn with_net(inner: Arc<HttpClient>, net: Arc<[String]>) -> Self {
    Self { inner, net }
  }

  /// The shared underlying context — lets the plugin dispatch wrap the
  /// session's `request` with a net allow-list without re-creating it.
  #[must_use]
  pub fn inner_arc(&self) -> Arc<HttpClient> {
    self.inner.clone()
  }

  /// The sandbox network policy for this binding: default-deny against
  /// `self.net` when an `allow.net` list is present, and the cloud
  /// instance-metadata endpoints blocked unconditionally (no automation
  /// targets them). Enforced in core on the initial URL, every redirect
  /// hop, and every resolved address.
  pub(crate) fn net_guard(&self) -> NetGuard {
    NetGuard {
      allowlist: (!self.net.is_empty()).then(|| self.net.clone()),
      block_metadata: true,
      block_private: false,
    }
  }

  /// Synchronous fast-fail on the initial URL so an allow-list
  /// violation throws before any I/O (the same check runs again in core
  /// for redirect hops). `Ok(())` when no allow-list, or the host
  /// matches; otherwise a JS-thrown error.
  fn guard(&self, url: &str) -> rquickjs::Result<()> {
    net_check(&self.net, url).map_err(|m| rquickjs::Error::new_from_js_message("request", "Error", m))
  }
}

/// Attach `g` to the per-request options (creating a default bag if the
/// caller passed none) so core enforces the sandbox network policy.
fn with_guard(opts: Option<RequestOptions>, g: NetGuard) -> RequestOptions {
  let mut o = opts.unwrap_or_default();
  o.net_guard = Some(g);
  o
}

/// Default-deny host check shared by the `request` binding and the
/// global `fetch` facade, delegating to the core allow-list semantics
/// (one implementation, in Rust core). `Ok(())` when `net` is empty
/// (unrestricted) or the URL's host matches an entry; otherwise an
/// `Err(message)`. Synchronous, before any network I/O. Metadata /
/// redirect-hop enforcement lives in core's [`NetGuard`].
pub(crate) fn net_check(net: &[String], url: &str) -> Result<(), String> {
  if net.is_empty() {
    return Ok(());
  }
  let host = ferridriver::http_client::host_of(url)
    .ok_or_else(|| format!("request to invalid/relative URL \"{url}\" is not permitted by allow.net"))?;
  if ferridriver::http_client::host_allowed(&host, net) {
    Ok(())
  } else {
    Err(format!("request host \"{host}\" is not in allow.net {net:?}"))
  }
}

#[rquickjs::methods]
impl HttpClientJs {
  #[qjs(rename = "get")]
  pub async fn get<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<HttpResponseJs> {
    self.guard(&url)?;
    let opts = Some(with_guard(parse_options(&ctx, options)?, self.net_guard()));
    let resp = self.inner.get(&url, opts).await.into_js()?;
    Ok(HttpResponseJs::new(resp))
  }

  #[qjs(rename = "post")]
  pub async fn post<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<HttpResponseJs> {
    self.guard(&url)?;
    let opts = Some(with_guard(parse_options(&ctx, options)?, self.net_guard()));
    let resp = self.inner.post(&url, opts).await.into_js()?;
    Ok(HttpResponseJs::new(resp))
  }

  #[qjs(rename = "put")]
  pub async fn put<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<HttpResponseJs> {
    self.guard(&url)?;
    let opts = Some(with_guard(parse_options(&ctx, options)?, self.net_guard()));
    let resp = self.inner.put(&url, opts).await.into_js()?;
    Ok(HttpResponseJs::new(resp))
  }

  #[qjs(rename = "delete")]
  pub async fn delete<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<HttpResponseJs> {
    self.guard(&url)?;
    let opts = Some(with_guard(parse_options(&ctx, options)?, self.net_guard()));
    let resp = self.inner.delete(&url, opts).await.into_js()?;
    Ok(HttpResponseJs::new(resp))
  }

  #[qjs(rename = "patch")]
  pub async fn patch<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<HttpResponseJs> {
    self.guard(&url)?;
    let opts = Some(with_guard(parse_options(&ctx, options)?, self.net_guard()));
    let resp = self.inner.patch(&url, opts).await.into_js()?;
    Ok(HttpResponseJs::new(resp))
  }

  #[qjs(rename = "head")]
  pub async fn head<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<HttpResponseJs> {
    self.guard(&url)?;
    let opts = Some(with_guard(parse_options(&ctx, options)?, self.net_guard()));
    let resp = self.inner.head(&url, opts).await.into_js()?;
    Ok(HttpResponseJs::new(resp))
  }

  /// Generic fetch — `options` may include `method` via `headers` only; this
  /// mirrors `RequestOptions` (no request overload for now — see the
  /// `PLAYWRIGHT_COMPAT.md` gap for `HttpClient.fetch(Request, ...)`).
  #[qjs(rename = "fetch")]
  pub async fn fetch<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<HttpResponseJs> {
    self.guard(&url)?;
    let opts = Some(with_guard(parse_options(&ctx, options)?, self.net_guard()));
    let resp = self.inner.fetch(&url, opts).await.into_js()?;
    Ok(HttpResponseJs::new(resp))
  }
}

// ── HttpResponseJs ────────────────────────────────────────────────────────────

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "HttpResponse")]
pub struct HttpResponseJs {
  #[qjs(skip_trace)]
  inner: HttpResponse,
}

impl HttpResponseJs {
  #[must_use]
  pub fn new(inner: HttpResponse) -> Self {
    Self { inner }
  }

  /// Clone of the wrapped core `HttpResponse` for cross-binding
  /// consumers (used by `expect()` to lift a `HttpResponseJs` into an
  /// `ApiResponse` assertion target).
  #[must_use]
  pub fn inner_clone(&self) -> HttpResponse {
    self.inner.clone()
  }
}

#[rquickjs::methods]
impl HttpResponseJs {
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
