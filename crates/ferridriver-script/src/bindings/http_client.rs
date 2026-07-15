//! `HttpClientJs` + `HttpResponseJs`: JS wrappers for HTTP calls from
//! the runner side (separate from the page's own network).

use std::sync::Arc;
use std::time::Duration;

use ferridriver::http_client::{HttpClient, HttpResponse, NetGuard, RequestOptions};
use rquickjs::function::Opt;
use rquickjs::promise::Promised;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use serde::Deserialize;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::serde_from_js;

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
  /// Host allow-list (extension `allow.net` capability). Empty =
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
  /// the per-tool `request` a extension handler receives when its manifest
  /// declares `allow.net`.
  #[must_use]
  pub fn with_net(inner: Arc<HttpClient>, net: Arc<[String]>) -> Self {
    Self { inner, net }
  }

  /// The shared underlying context — lets the extension dispatch wrap the
  /// session's `request` with a net allow-list without re-creating it.
  #[must_use]
  pub fn inner_arc(&self) -> Arc<HttpClient> {
    self.inner.clone()
  }

  /// The allow-list this binding enforces right now: an instance list
  /// (a net-restricted tool's `request` arg carries its grant wherever
  /// the object travels), else the session's *active* tool policy — so
  /// the ungoverned global `request` is bound by `allow.net` exactly
  /// like `fetch` is, and a restricted handler cannot widen its grant by
  /// reaching for `globalThis.request` instead of its guarded arg.
  fn effective_net(&self, ctx: &Ctx<'_>) -> Option<Arc<[String]>> {
    if !self.net.is_empty() {
      return Some(self.net.clone());
    }
    crate::bindings::fetch::active_net(ctx)
  }

  /// Shared body of every HTTP method. Snapshots the effective policy
  /// NOW — synchronously, while this call is still on the caller's
  /// stack — because an `async fn` method body first polls on the VM
  /// executor, outside the dispatch bracket, where the resting policy
  /// (unrestricted) would be read instead of the calling tool's. The
  /// allow-list check itself runs inside the returned promise so a
  /// violation is a rejection (not a synchronous throw), and core
  /// re-enforces it on every redirect hop and resolved address via
  /// [`NetGuard`]; the metadata endpoints are blocked unconditionally.
  fn dispatch<'js>(
    &self,
    ctx: Ctx<'js>,
    verb: Verb,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    let net = self.effective_net(&ctx);
    let opts = parse_options(&ctx, options)?;
    let inner = self.inner.clone();
    Ok(Promised::from(async move {
      if let Some(list) = net.as_deref() {
        net_check(list, &url).map_err(|m| rquickjs::Error::new_from_js_message("request", "Error", m))?;
      }
      let guard = NetGuard {
        allowlist: net,
        block_metadata: true,
        block_private: false,
      };
      let opts = Some(with_guard(opts, guard));
      let resp = match verb {
        Verb::Get => inner.get(&url, opts).await,
        Verb::Post => inner.post(&url, opts).await,
        Verb::Put => inner.put(&url, opts).await,
        Verb::Delete => inner.delete(&url, opts).await,
        Verb::Patch => inner.patch(&url, opts).await,
        Verb::Head => inner.head(&url, opts).await,
        Verb::Fetch => inner.fetch(&url, opts).await,
      }
      .into_js_with(&ctx)?;
      Ok(HttpResponseJs::new(resp))
    }))
  }
}

#[derive(Clone, Copy)]
enum Verb {
  Get,
  Post,
  Put,
  Delete,
  Patch,
  Head,
  Fetch,
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
  pub fn get<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    self.dispatch(ctx, Verb::Get, url, options)
  }

  #[qjs(rename = "post")]
  pub fn post<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    self.dispatch(ctx, Verb::Post, url, options)
  }

  #[qjs(rename = "put")]
  pub fn put<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    self.dispatch(ctx, Verb::Put, url, options)
  }

  #[qjs(rename = "delete")]
  pub fn delete<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    self.dispatch(ctx, Verb::Delete, url, options)
  }

  #[qjs(rename = "patch")]
  pub fn patch<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    self.dispatch(ctx, Verb::Patch, url, options)
  }

  #[qjs(rename = "head")]
  pub fn head<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    self.dispatch(ctx, Verb::Head, url, options)
  }

  /// Generic fetch — `options` may include `method` via `headers` only; this
  /// mirrors `RequestOptions` (no request overload for now — see the
  /// `docs/PLAYWRIGHT-PARITY-BACKLOG.md` gap for `HttpClient.fetch(Request, ...)`).
  #[qjs(rename = "fetch")]
  pub fn fetch<'js>(
    &self,
    ctx: Ctx<'js>,
    url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    self.dispatch(ctx, Verb::Fetch, url, options)
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

  /// Playwright: `apiResponse.serverAddr(): Promise<{ ipAddress, port } | null>`.
  /// Resolved peer address, or `null` when the transport didn't surface one.
  #[qjs(rename = "serverAddr")]
  pub fn server_addr<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    match self.inner.server_addr() {
      Some(addr) => {
        let obj = rquickjs::Object::new(ctx.clone())?;
        obj.set("ipAddress", addr.ip_address.clone())?;
        obj.set("port", addr.port)?;
        Ok(obj.into_value())
      },
      None => Ok(Value::new_null(ctx)),
    }
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
  pub fn text(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    self.inner.text().into_js_with(&ctx)
  }

  /// Response body parsed as JSON.
  #[qjs(rename = "json")]
  pub fn json<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    // Parse the raw body straight into a JS value with QuickJS's C JSON
    // parser — no serde_json::Value middle allocation. `json_parse`
    // does not touch the JS `JSON` global, so a reassigned
    // `globalThis.JSON` cannot affect it.
    let text = self.inner.text().into_js_with(&ctx)?;
    ctx.json_parse(text)
  }
}
