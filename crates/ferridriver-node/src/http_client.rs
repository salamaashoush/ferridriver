//! NAPI bindings for the HTTP client (backs the Playwright-style
//! `request` API and the WHATWG `fetch` global).

use crate::error::IntoNapi;
use napi::Result;
use napi::bindgen_prelude::PromiseRaw;
use napi_derive::napi;

/// Playwright's `request.newContext(options?)` bag.
#[napi(object, js_name = "NewContextOptions")]
#[derive(Debug, Clone, Default)]
pub struct HttpClientOptions {
  /// Base URL prepended to relative paths.
  // napi camelCases these to `baseUrl` / `extraHttpHeaders` /
  // `ignoreHttpsErrors`; Playwright keeps the acronyms upper-case, and a
  // mis-spelled key binds to nothing and is silently ignored.
  #[napi(js_name = "baseURL")]
  pub base_url: Option<String>,
  /// Headers every request sends.
  #[napi(js_name = "extraHTTPHeaders")]
  pub extra_http_headers: Option<std::collections::HashMap<String, String>>,
  /// Default timeout in milliseconds.
  pub timeout: Option<f64>,
  /// Ignore HTTPS certificate errors.
  #[napi(js_name = "ignoreHTTPSErrors")]
  pub ignore_https_errors: Option<bool>,
  /// What answers a Basic challenge: one set of credentials or a list.
  #[napi(
    ts_type = "{ username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' } | Array<{ username: string; password: string; origin?: string; send?: 'always' | 'unauthorized' }>"
  )]
  pub http_credentials: Option<crate::context::NapiHttpCredentialsInput>,
  /// `user-agent` every request sends unless it names its own.
  pub user_agent: Option<String>,
  /// Throw on a 4xx or 5xx answer unless the request says otherwise.
  pub fail_on_status_code: Option<bool>,
  /// Redirects a request follows unless it says otherwise.
  pub max_redirects: Option<u32>,
  /// A `storageState` file path or object: its cookies seed the context.
  #[napi(ts_type = "string | { cookies?: any[]; origins?: any[] }")]
  pub storage_state: Option<serde_json::Value>,
  /// An HTTP(S) or SOCKS proxy every request goes through, credentials
  /// included.
  pub proxy: Option<crate::context::NapiProxyConfig>,
  /// Not supported yet for API contexts; refused rather than ignored.
  #[napi(ts_type = "Array<Record<string, unknown>>")]
  pub client_certificates: Option<serde_json::Value>,
}

/// Per-request options. Mirrors Playwright's `APIRequestContext`
/// option bag (`packages/playwright-core/types/types.d.ts`): `headers`
/// a plain object, `params`/`form` plain objects with
/// string/number/boolean values, `timeout` in milliseconds.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
  /// HTTP method override (`fetch` only; the verb helpers set it).
  pub method: Option<String>,
  /// Extra headers for this request.
  pub headers: Option<std::collections::HashMap<String, String>>,
  /// Request body: a string is sent raw, any other serializable value
  /// is sent as JSON (Playwright's `data` routing).
  pub data: Option<serde_json::Value>,
  /// URL-encoded form data.
  #[napi(ts_type = "Record<string, string | number | boolean>")]
  pub form: Option<std::collections::HashMap<String, serde_json::Value>>,
  /// Query string parameters.
  #[napi(ts_type = "Record<string, string | number | boolean>")]
  pub params: Option<std::collections::HashMap<String, serde_json::Value>>,
  /// Timeout in milliseconds.
  pub timeout: Option<f64>,
  /// Fail with error on 4xx/5xx.
  pub fail_on_status_code: Option<bool>,
  /// Max redirects.
  pub max_redirects: Option<i32>,
  #[napi(ts_type = "'follow' | 'manual' | 'error'")]
  pub redirect: Option<String>,
  /// Retry on a connection reset up to this many times.
  pub max_retries: Option<i32>,
  /// Per-request override of the client-level TLS posture.
  #[napi(js_name = "ignoreHTTPSErrors")]
  pub ignore_https_errors: Option<bool>,
  /// `multipart/form-data` body. A value is a scalar text field, or
  /// `{ name, mimeType, buffer }` for a file part.
  #[napi(
    ts_type = "Record<string, string | number | boolean | { name: string, mimeType?: string, buffer: Buffer | string }>"
  )]
  pub multipart: Option<std::collections::HashMap<String, serde_json::Value>>,
}

/// Lower a Playwright `params` / `form` map through the ONE core
/// implementation, so this binding and the QuickJS one cannot disagree
/// about what a scalar bag means.
fn scalar_map_to_pairs(
  field: &str,
  map: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<Vec<(String, String)>> {
  ferridriver::http_client::RequestOptions::scalar_map_to_pairs(field, map.iter().map(|(k, v)| (k.clone(), v.clone())))
    .map_err(napi::Error::from_reason)
}

impl FetchOptions {
  fn to_core(&self) -> Result<ferridriver::http_client::RequestOptions> {
    let (data, json_data) = ferridriver::http_client::RequestOptions::split_data(self.data.clone(), None);
    Ok(ferridriver::http_client::RequestOptions {
      method: self.method.as_ref().map(|m| m.to_ascii_uppercase()),
      headers: self
        .headers
        .as_ref()
        .map(|h| h.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
      json_data,
      data,
      form: self.form.as_ref().map(|f| scalar_map_to_pairs("form", f)).transpose()?,
      params: self
        .params
        .as_ref()
        .map(|p| scalar_map_to_pairs("params", p))
        .transpose()?,
      timeout: self.timeout.map(|t| std::time::Duration::from_millis(t as u64)),
      fail_on_status_code: self.fail_on_status_code,
      max_redirects: self.max_redirects.map(|m| m as u32),
      redirect: self
        .redirect
        .as_deref()
        .map(ferridriver::http_client::RequestOptions::parse_redirect)
        .transpose()
        .map_err(napi::Error::from_reason)?
        .unwrap_or_default(),
      max_retries: self.max_retries.map(|m| m as u32),
      ignore_https_errors: self.ignore_https_errors,
      multipart: self
        .multipart
        .as_ref()
        .map(|m| {
          ferridriver::http_client::MultipartField::from_json_map(m.iter().map(|(k, v)| (k.clone(), v.clone())))
            .map_err(napi::Error::from_reason)
        })
        .transpose()?,
      // The Node binding is the trusted Playwright-in-Rust surface, not
      // the script sandbox — no network guard is imposed here.
      net_guard: None,
      ..Default::default()
    })
  }
}

/// Layer explicit `options` over a `Request`-derived base: anything the
/// caller set wins, the rest falls through to the request's own values.
/// Mirrors the QuickJS binding's `merge_over`.
fn merge_over(
  base: Option<ferridriver::http_client::RequestOptions>,
  options: Option<ferridriver::http_client::RequestOptions>,
) -> Option<ferridriver::http_client::RequestOptions> {
  let (base, options) = match (base, options) {
    (Some(base), Some(options)) => (base, options),
    (base, options) => return base.or(options),
  };
  Some(ferridriver::http_client::RequestOptions {
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

/// API response from an HTTP request.
#[napi(js_name = "APIResponse")]
pub struct HttpResponse {
  pub(crate) inner: ferridriver::http_client::HttpResponse,
}

/// What Playwright's `APIResponse` hands back as a promise and this
/// response already holds: its body is buffered when it is made. An
/// error rejects the promise, as a disposed body does upstream.
fn settled<T>(env: &napi::Env, value: Result<T>) -> Result<PromiseRaw<'_, T>>
where
  T: napi::bindgen_prelude::ToNapiValue + Send + 'static,
{
  env.spawn_future(async move { value })
}

#[napi]
impl HttpResponse {
  #[napi]
  pub fn redirected(&self) -> bool {
    self.inner.redirected()
  }

  #[napi]
  pub fn unfollowed_redirect(&self) -> bool {
    self.inner.unfollowed_redirect()
  }

  /// Playwright: `apiResponse.status(): number`.
  #[napi]
  pub fn status(&self) -> i32 {
    self.inner.status() as i32
  }

  /// Playwright: `apiResponse.statusText(): string` (e.g. "OK").
  #[napi]
  pub fn status_text(&self) -> String {
    self.inner.status_text().to_string()
  }

  /// Playwright: `apiResponse.url(): string`, the URL after redirects.
  #[napi]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Whether the response status is 200-299.
  #[napi]
  pub fn ok(&self) -> bool {
    self.inner.ok()
  }

  /// Playwright: `apiResponse.timing(): ResourceTiming`. Milliseconds
  /// from `startTime`; `-1` for a phase that did not happen.
  #[napi]
  pub fn timing(&self) -> crate::network::RequestTiming {
    self.inner.timing().into()
  }

  /// Playwright: `apiResponse.securityDetails(): Promise<SecurityDetails | null>`.
  /// The server certificate's subject, issuer and validity; `null` for
  /// a response that did not arrive over TLS.
  #[napi(ts_return_type = "Promise<SecurityDetails | null>")]
  pub fn security_details<'env>(
    &self,
    env: &'env napi::Env,
  ) -> Result<PromiseRaw<'env, Option<crate::network::SecurityDetails>>> {
    settled(env, Ok(self.inner.security_details().map(Into::into)))
  }

  /// Playwright: `apiResponse.serverAddr(): Promise<RemoteAddr | null>`.
  /// Resolved peer address, or `null` when the transport didn't surface
  /// one.
  #[napi(ts_return_type = "Promise<{ ipAddress: string, port: number } | null>")]
  pub fn server_addr<'env>(
    &self,
    env: &'env napi::Env,
  ) -> Result<PromiseRaw<'env, Option<crate::network::RemoteAddr>>> {
    let addr = self.inner.server_addr().map(|a| crate::network::RemoteAddr {
      ip_address: a.ip_address.clone(),
      port: u32::from(a.port),
    });
    settled(env, Ok(addr))
  }

  /// Playwright: `apiResponse.headers(): { [key: string]: string }` —
  /// lowercased names, duplicates combined.
  #[napi(ts_return_type = "Record<string, string>")]
  pub fn headers(&self) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = self
      .inner
      .headers_object()
      .into_iter()
      .map(|(k, v)| (k, serde_json::Value::String(v)))
      .collect();
    serde_json::Value::Object(map)
  }

  /// Playwright: `apiResponse.headersArray(): HeadersArray` — every
  /// header verbatim, duplicates and original casing preserved.
  #[napi(ts_return_type = "Array<{ name: string, value: string }>")]
  pub fn headers_array(&self) -> serde_json::Value {
    serde_json::Value::Array(
      self
        .inner
        .headers()
        .iter()
        .map(|(name, value)| serde_json::json!({ "name": name, "value": value }))
        .collect(),
    )
  }

  /// Combined value of a single header, or `null` when absent.
  #[napi]
  pub fn header(&self, name: String) -> Option<String> {
    self.inner.header(&name)
  }

  /// Playwright: `apiResponse.text(): Promise<string>`.
  #[napi]
  pub fn text<'env>(&self, env: &'env napi::Env) -> Result<PromiseRaw<'env, String>> {
    settled(env, self.inner.text().into_napi())
  }

  /// Playwright: `apiResponse.json(): Promise<Serializable>`.
  #[napi(ts_return_type = "Promise<any>")]
  pub fn json<'env>(&self, env: &'env napi::Env) -> Result<PromiseRaw<'env, serde_json::Value>> {
    settled(env, self.inner.json_value().into_napi())
  }

  /// Playwright: `apiResponse.body(): Promise<Buffer>`.
  #[napi]
  pub fn body<'env>(&self, env: &'env napi::Env) -> Result<PromiseRaw<'env, napi::bindgen_prelude::Buffer>> {
    settled(env, self.inner.body().map(|b| b.to_vec().into()).into_napi())
  }

  /// Playwright: `apiResponse.dispose(): Promise<void>` — release the
  /// buffered body. Status, headers and URL stay readable; body
  /// accessors then reject.
  #[napi(ts_return_type = "Promise<void>")]
  pub fn dispose<'env>(&mut self, env: &'env napi::Env) -> Result<PromiseRaw<'env, ()>> {
    self.inner.dispose();
    settled(env, Ok(()))
  }
}

/// Playwright's `APIRequestContext`: the HTTP client behind `request`
/// and `page.request`.
#[napi(js_name = "APIRequestContext")]
pub struct HttpClient {
  inner: ferridriver::http_client::HttpClient,
}

impl HttpClient {
  pub(crate) fn wrap(inner: ferridriver::http_client::HttpClient) -> Self {
    Self { inner }
  }
}

/// Playwright: `request.newContext(options?): Promise<APIRequestContext>`,
/// `request` being the module's `APIRequest`.
#[napi(namespace = "request", ts_return_type = "Promise<APIRequestContext>")]
pub async fn new_context(options: Option<HttpClientOptions>) -> Result<HttpClient> {
  let opts = options.unwrap_or_default();
  let proxy = opts.proxy.map(|p| ferridriver::options::ProxyConfig {
    server: p.server,
    bypass: p.bypass,
    username: p.username,
    password: p.password,
  });
  if let Some(proxy) = &proxy {
    proxy.validate().into_napi()?;
  }
  if opts.client_certificates.is_some() {
    return Err(crate::error::to_napi(ferridriver::error::FerriError::unsupported(
      "clientCertificates is not supported for request.newContext yet",
    )));
  }
  let storage = opts
    .storage_state
    .map(|input| {
      let value = crate::context::resolve_storage_state_input(input).map_err(napi::Error::from_reason)?;
      serde_json::from_value::<ferridriver::options::StorageState>(value)
        .map_err(|e| napi::Error::from_reason(format!("storageState: {e}")))
    })
    .transpose()?;
  let core_opts = ferridriver::http_client::HttpClientOptions {
    base_url: opts.base_url,
    extra_http_headers: opts.extra_http_headers.unwrap_or_default().into_iter().collect(),
    timeout: opts
      .timeout
      .map(|t| std::time::Duration::from_millis(crate::types::f64_to_u64(t))),
    ignore_https_errors: opts.ignore_https_errors.unwrap_or(false),
    http_credentials: opts
      .http_credentials
      .and_then(crate::context::http_credentials_list)
      .unwrap_or_default(),
    user_agent: opts.user_agent,
    fail_on_status_code: opts.fail_on_status_code,
    max_redirects: opts.max_redirects,
    proxy,
  };
  let mut client = ferridriver::http_client::HttpClient::new(core_opts);
  if let Some(state) = storage {
    client = client.with_storage_state(state).into_napi()?;
  }
  Ok(HttpClient::wrap(client))
}

#[napi]
impl HttpClient {
  /// Playwright: `apiRequestContext.storageState(options?)`.
  #[napi]
  pub async fn storage_state(&self) -> Result<crate::context::NapiStorageState> {
    let state = self.inner.storage_state().await.into_napi()?;
    crate::context::NapiStorageState::try_from(state)
  }

  /// Send a GET request.
  #[napi]
  pub async fn get(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core()).transpose()?;
    let resp = self.inner.get(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a POST request.
  #[napi]
  pub async fn post(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core()).transpose()?;
    let resp = self.inner.post(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a PUT request.
  #[napi]
  pub async fn put(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core()).transpose()?;
    let resp = self.inner.put(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a DELETE request.
  #[napi]
  pub async fn delete(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core()).transpose()?;
    let resp = self.inner.delete(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a PATCH request.
  #[napi]
  pub async fn patch(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core()).transpose()?;
    let resp = self.inner.patch(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a HEAD request.
  #[napi]
  pub async fn head(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core()).transpose()?;
    let resp = self.inner.head(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Mirrors Playwright `apiRequestContext.fetch(urlOrRequest, options?)`.
  ///
  /// A page-network `Request` contributes its URL, method, headers and
  /// post body; anything also given in `options` wins.
  #[napi(ts_args_type = "urlOrRequest: string | Request, options?: FetchOptions")]
  pub async fn fetch(
    &self,
    url_or_request: napi::Either<String, &crate::network::Request>,
    options: Option<FetchOptions>,
  ) -> Result<HttpResponse> {
    let (url, base) = match url_or_request {
      napi::Either::A(url) => (url, None),
      napi::Either::B(req) => (
        req.url(),
        Some(ferridriver::http_client::RequestOptions {
          method: Some(req.method()),
          headers: Some(ferridriver::http_client::replayable_headers(
            req.inner.headers().into_iter().collect(),
          )),
          data: req.inner.post_data_buffer(),
          ..Default::default()
        }),
      ),
    };
    let opts = merge_over(base, options.map(|o| o.to_core()).transpose()?);
    let resp = self.inner.fetch(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Playwright: `apiRequestContext.dispose(): Promise<void>`. The
  /// client is shared with whatever vended it, so there is nothing of
  /// its own to tear down.
  #[napi(ts_return_type = "Promise<void>")]
  pub fn dispose<'env>(&self, env: &'env napi::Env) -> Result<PromiseRaw<'env, ()>> {
    settled(env, Ok(()))
  }

  /// Playwright: `apiRequestContext.cookies(urls?: string | string[])`.
  /// The cookies of the browser context this client is bound to, or of
  /// its own jar.
  #[napi(ts_args_type = "urls?: string | string[]")]
  pub async fn cookies(
    &self,
    urls: Option<napi::Either<String, Vec<String>>>,
  ) -> Result<Vec<crate::types::CookieData>> {
    let cookies = self
      .inner
      .cookies(&crate::context::cookie_urls(urls))
      .await
      .into_napi()?;
    Ok(cookies.iter().map(crate::types::CookieData::from).collect())
  }

  /// Playwright: `apiRequestContext.addCookies(cookies)`.
  #[napi]
  pub async fn add_cookies(&self, cookies: Vec<crate::types::CookieData>) -> Result<()> {
    let cookies = cookies.iter().map(ferridriver::backend::CookieData::from).collect();
    self.inner.add_cookies(cookies).await.into_napi()
  }

  /// Playwright: `apiRequestContext.clearCookies(options?: { name?, domain?, path? })`.
  #[napi]
  pub async fn clear_cookies(&self, options: Option<crate::types::ClearCookieOptions>) -> Result<()> {
    let filter: ferridriver::backend::ClearCookieOptions = options.map(Into::into).unwrap_or_default();
    self.inner.clear_cookies(&filter).await.into_napi()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn options_it_cannot_honour_are_refused() {
    let certificates = HttpClientOptions {
      client_certificates: Some(serde_json::json!([])),
      ..HttpClientOptions::default()
    };
    let refused = new_context(Some(certificates)).await.err().map(|e| e.reason);
    assert!(refused.is_some_and(|reason| reason.contains("clientCertificates is not supported")));
    assert!(new_context(None).await.is_ok());
  }

  #[tokio::test]
  async fn a_proxy_that_names_no_proxy_is_refused_where_it_is_given() {
    let proxy = |server: &str| HttpClientOptions {
      proxy: Some(crate::context::NapiProxyConfig {
        server: server.into(),
        bypass: None,
        username: None,
        password: None,
      }),
      ..HttpClientOptions::default()
    };
    let refused = new_context(Some(proxy("ftp://proxy.example.com")))
      .await
      .err()
      .map(|e| e.reason);
    assert!(refused.is_some_and(|reason| reason.contains("proxy")));
    assert!(new_context(Some(proxy("http://proxy.example.com:3128"))).await.is_ok());
  }
}
