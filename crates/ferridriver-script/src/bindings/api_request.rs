//! `APIRequestContextJs` + `APIResponseJs`: JS wrappers for HTTP calls from
//! the runner side (separate from the page's own network).

use std::sync::Arc;
use std::time::Duration;

use ferridriver::api_request::{APIRequestContext, APIResponse, RequestOptions};
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use serde::Deserialize;

use crate::bindings::convert::{FerriResultExt, serde_from_js, serde_to_js};

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
}

impl APIRequestContextJs {
  #[must_use]
  pub fn new(inner: Arc<APIRequestContext>) -> Self {
    Self { inner }
  }
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
    let headers: Vec<serde_json::Value> = self
      .inner
      .headers()
      .iter()
      .map(|(n, v)| serde_json::json!({ "name": n, "value": v }))
      .collect();
    serde_to_js(&ctx, &headers)
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
    let value = self.inner.json_value().into_js()?;
    serde_to_js(&ctx, &value)
  }
}
