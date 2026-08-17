//! `HttpClientJs` + `HttpResponseJs`: JS wrappers for HTTP calls from
//! the runner side (separate from the page's own network).

use std::sync::Arc;
use std::time::Duration;

use either::Either;
use ferridriver::http_client::{HttpClient, HttpResponse, NetGuard, RequestOptions};
use rquickjs::function::Opt;
use rquickjs::promise::Promised;
use rquickjs::{Class, Ctx, JsLifetime, Value, class::Trace};
use rustc_hash::FxHashMap;
use serde::Deserialize;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::serde_from_js;

/// Shape of per-request options accepted from JS. Playwright's
/// option-bag shapes (`packages/playwright-core/src/client/fetch.ts`,
/// types `APIRequestContext.get(url, options)`): camelCase keys,
/// `headers` a plain object, `params`/`form` plain objects with
/// string/number/boolean values, `timeout` in milliseconds. `json` is a
/// ferridriver extension (explicit JSON body; Playwright routes
/// serializable bodies through `data`).
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct JsRequestOptions {
  method: Option<String>,
  headers: Option<FxHashMap<String, String>>,
  data: Option<serde_json::Value>,
  json: Option<serde_json::Value>,
  form: Option<FxHashMap<String, serde_json::Value>>,
  params: Option<FxHashMap<String, serde_json::Value>>,
  timeout: Option<u64>,
  fail_on_status_code: Option<bool>,
  max_redirects: Option<u32>,
  max_retries: Option<u32>,
  // serde's camelCase would spell this `ignoreHttpsErrors`; Playwright's
  // option keeps the acronym upper-case, so it must be named explicitly
  // or the key silently never binds and the client default wins.
  #[serde(rename = "ignoreHTTPSErrors")]
  ignore_https_errors: Option<bool>,
  multipart: Option<serde_json::Map<String, serde_json::Value>>,
}

impl JsRequestOptions {
  fn into_core(self) -> Result<RequestOptions, String> {
    let (data, json_data) = RequestOptions::split_data(self.data, self.json);
    Ok(RequestOptions {
      method: self.method.map(|m| m.to_ascii_uppercase()),
      headers: self.headers.map(|h| h.into_iter().collect()),
      data,
      json_data,
      form: self
        .form
        .map(|f| RequestOptions::scalar_map_to_pairs("form", f))
        .transpose()?,
      params: self
        .params
        .map(|p| RequestOptions::scalar_map_to_pairs("params", p))
        .transpose()?,
      timeout: self.timeout.map(Duration::from_millis),
      fail_on_status_code: self.fail_on_status_code,
      max_redirects: self.max_redirects,
      max_retries: self.max_retries,
      ignore_https_errors: self.ignore_https_errors,
      multipart: self
        .multipart
        .map(ferridriver::http_client::MultipartField::from_json_map)
        .transpose()?,
      // Set by `with_guard` after parsing — never from JS input.
      net_guard: None,
      ..Default::default()
    })
  }
}

/// Layer explicit `options` over a `Request`-derived base: anything the
/// caller set wins, the rest falls through to the request's own values.
fn merge_over(base: Option<RequestOptions>, options: Option<RequestOptions>) -> Option<RequestOptions> {
  let (base, options) = match (base, options) {
    (Some(base), Some(options)) => (base, options),
    (base, options) => return base.or(options),
  };
  Some(RequestOptions {
    method: options.method.or(base.method),
    headers: match (options.headers, base.headers) {
      (Some(explicit), Some(inherited)) => {
        // Explicit headers win per name; the request's others survive.
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
    // A body given in `options` replaces the request's entirely, in any
    // of its forms — mixing them would produce two bodies.
    data: options.data.or_else(|| {
      (options.json_data.is_none() && options.form.is_none() && options.multipart.is_none()).then_some(base.data)?
    }),
    ..options
  })
}

fn parse_options<'js>(ctx: &Ctx<'js>, value: Opt<Value<'js>>) -> rquickjs::Result<Option<RequestOptions>> {
  match value.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => {
      let parsed: JsRequestOptions = serde_from_js(ctx, v)?;
      let core = parsed
        .into_core()
        .map_err(|m| rquickjs::Error::new_from_js_message("options", "RequestOptions", m))?;
      Ok(Some(core))
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
    self.dispatch_with(ctx, verb, url, options, None)
  }

  /// [`Self::dispatch`] with a `Request`-derived base the caller's
  /// `options` are layered over.
  fn dispatch_with<'js>(
    &self,
    ctx: Ctx<'js>,
    verb: Verb,
    url: String,
    options: Opt<Value<'js>>,
    base: Option<RequestOptions>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    let net = self.effective_net(&ctx);
    let opts = merge_over(base, parse_options(&ctx, options)?);
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

  /// Playwright: `fetch(urlOrRequest: string | Request, options?)`.
  ///
  /// A page-network `Request` contributes its URL, method, headers and
  /// post body; anything the caller also passes in `options` wins, per
  /// Playwright's `_innerFetch`.
  /// The union is spelled as `Either`, which converts by trying a
  /// `Request` first and falling back to a string — so an argument that
  /// is neither is rejected by the conversion with rquickjs's own type
  /// error, instead of by a hand-written downcast chain.
  #[qjs(rename = "fetch")]
  pub fn fetch<'js>(
    &self,
    ctx: Ctx<'js>,
    url_or_request: Either<Class<'js, crate::bindings::network::RequestJs>, String>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Promised<impl std::future::Future<Output = rquickjs::Result<HttpResponseJs>> + 'js>> {
    let (url, from_request) = match url_or_request {
      Either::Left(request) => {
        let request = request.borrow();
        (request.url(), Some(request_defaults(&request)))
      },
      Either::Right(url) => (url, None),
    };
    self.dispatch_with(ctx, Verb::Fetch, url, options, from_request)
  }

  /// Playwright: `dispose()` — release the context's resources. The
  /// underlying client is reference-counted and shared with the browser
  /// context that vended it, so this drops this binding's handle rather
  /// than tearing the shared pool down under other holders.
  #[qjs(rename = "dispose")]
  pub fn dispose(&self) {}
}

/// Method, headers and body carried over from a page-network `Request`
/// passed to `fetch`.
fn request_defaults(req: &crate::bindings::network::RequestJs) -> RequestOptions {
  RequestOptions {
    method: Some(req.method()),
    headers: Some(replayable_headers(req.header_pairs())),
    data: req.post_data_bytes(),
    ..Default::default()
  }
}

/// Strip the headers that describe the connection rather than the
/// request: the client recomputes them.
///
/// `content-length` in particular MUST go — a replay whose body the
/// capture did not carry would otherwise announce a length the server
/// then waits forever to receive.
fn replayable_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
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

  /// All response headers as a flat object: lowercased names, duplicates
  /// combined (Playwright's `apiResponse.headers()`).
  #[qjs(rename = "headers")]
  pub fn headers<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let obj = rquickjs::Object::new(ctx.clone())?;
    for (name, value) in self.inner.headers_object() {
      obj.set(name, value)?;
    }
    Ok(obj.into_value())
  }

  /// Combined value of a single header, or `null` if absent.
  #[qjs(rename = "header")]
  pub fn header<'js>(&self, ctx: Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    crate::bindings::convert::nullable(&ctx, self.inner.header(&name))
  }

  /// Playwright: `apiResponse.dispose()` — release the buffered body.
  /// Status, headers and URL stay readable; body accessors then throw.
  #[qjs(rename = "dispose")]
  pub fn dispose(&mut self) {
    self.inner.dispose();
  }

  /// Playwright: `apiResponse.body(): Promise<Buffer>` — raw bytes as
  /// a `Uint8Array`.
  ///
  /// The view is built directly over the response's own buffer instead
  /// of copying it twice (slice -> `Vec` -> JS heap), which matters for
  /// a large body. rquickjs keeps the `Bytes` alive for as long as the
  /// `ArrayBuffer` lives, and marks it immutable — so writing through
  /// the returned array throws rather than corrupting a buffer other
  /// readers of the same response still share.
  #[qjs(rename = "body")]
  pub fn body<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let bytes = self.inner.body_shared().into_js_with(&ctx)?;
    let buffer = rquickjs::ArrayBuffer::from_source_immutable(ctx.clone(), bytes)?;
    Ok(rquickjs::TypedArray::<u8>::from_arraybuffer(buffer)?.into_value())
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
