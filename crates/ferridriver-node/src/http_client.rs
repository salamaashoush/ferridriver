//! NAPI bindings for the HTTP client (backs the Playwright-style
//! `request` API and the WHATWG `fetch` global).

use crate::error::IntoNapi;
use napi::Result;
use napi_derive::napi;

/// Options for creating an `HttpClient`.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct HttpClientOptions {
  /// Base URL prepended to relative paths.
  pub base_url: Option<String>,
  /// Default headers as `[[key, value], ...]`.
  pub extra_http_headers: Option<Vec<Vec<String>>>,
  /// Default timeout in milliseconds.
  pub timeout: Option<f64>,
  /// Ignore HTTPS certificate errors.
  pub ignore_https_errors: Option<bool>,
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
  /// Retry on a connection reset up to this many times.
  pub max_retries: Option<i32>,
  /// Per-request override of the client-level TLS posture.
  pub ignore_https_errors: Option<bool>,
  /// `multipart/form-data` body. A value is a scalar text field, or
  /// `{ name, mimeType, buffer }` for a file part.
  #[napi(
    ts_type = "Record<string, string | number | boolean | { name: string, mimeType?: string, buffer: Buffer | string }>"
  )]
  pub multipart: Option<std::collections::HashMap<String, serde_json::Value>>,
}

/// Lower a `params`/`form` scalar to its string form. Playwright's
/// types admit `string | number | boolean` — anything else is a caller
/// error.
fn scalar_to_string(field: &str, key: &str, value: &serde_json::Value) -> Result<String> {
  match value {
    serde_json::Value::String(s) => Ok(s.clone()),
    serde_json::Value::Number(n) => Ok(n.to_string()),
    serde_json::Value::Bool(b) => Ok(b.to_string()),
    other => Err(napi::Error::from_reason(format!(
      "{field}[{key:?}] must be a string, number, or boolean (got {other})"
    ))),
  }
}

fn scalar_map_to_pairs(
  field: &str,
  map: &std::collections::HashMap<String, serde_json::Value>,
) -> Result<Vec<(String, String)>> {
  map
    .iter()
    .map(|(k, v)| scalar_to_string(field, k, v).map(|s| (k.clone(), s)))
    .collect()
}

impl FetchOptions {
  fn to_core(&self) -> Result<ferridriver::http_client::RequestOptions> {
    // Playwright's `data`: a string is a raw body, any other
    // serializable value goes as JSON.
    let (data, json_data) = match &self.data {
      Some(serde_json::Value::String(s)) => (Some(s.clone().into_bytes()), None),
      Some(value) => (None, Some(value.clone())),
      None => (None, None),
    };
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

/// Strip the headers that describe the connection rather than the
/// request: the client recomputes them. `content-length` in particular
/// MUST go — a replay whose body the capture did not carry would
/// otherwise announce a length the server then waits forever to receive.
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
#[napi]
pub struct HttpResponse {
  inner: ferridriver::http_client::HttpResponse,
}

#[napi]
impl HttpResponse {
  /// HTTP status code.
  #[napi(getter)]
  pub fn status(&self) -> i32 {
    self.inner.status() as i32
  }

  /// HTTP status text (e.g., "OK", "Not Found").
  #[napi(getter)]
  pub fn status_text(&self) -> String {
    self.inner.status_text().to_string()
  }

  /// Final URL after redirects.
  #[napi(getter)]
  pub fn url(&self) -> String {
    self.inner.url().to_string()
  }

  /// Whether the response status is 200-299.
  #[napi]
  pub fn ok(&self) -> bool {
    self.inner.ok()
  }

  /// Playwright: `apiResponse.serverAddr(): Promise<RemoteAddr | null>`.
  /// Resolved peer address, or `null` when the transport didn't surface
  /// one.
  #[napi(ts_return_type = "{ ipAddress: string, port: number } | null")]
  pub fn server_addr(&self) -> Option<crate::network::RemoteAddr> {
    self.inner.server_addr().map(|a| crate::network::RemoteAddr {
      ip_address: a.ip_address.clone(),
      port: u32::from(a.port),
    })
  }

  /// Response headers as a JSON object.
  #[napi]
  pub fn headers(&self) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = self
      .inner
      .headers()
      .iter()
      .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
      .collect();
    serde_json::Value::Object(map)
  }

  /// Response body as string.
  #[napi]
  pub fn text(&self) -> Result<String> {
    self.inner.text().into_napi()
  }

  /// Response body parsed as JSON.
  #[napi]
  pub fn json(&self) -> Result<serde_json::Value> {
    self.inner.json_value().into_napi()
  }

  /// Raw response body as Buffer.
  #[napi]
  pub fn body(&self) -> napi::bindgen_prelude::Buffer {
    self.inner.body().to_vec().into()
  }
}

/// A general HTTP client backing `fetch` and the `request` API.
#[napi]
pub struct HttpClient {
  inner: ferridriver::http_client::HttpClient,
}

impl HttpClient {
  pub(crate) fn wrap(inner: ferridriver::http_client::HttpClient) -> Self {
    Self { inner }
  }
}

#[napi]
impl HttpClient {
  /// Create a new HTTP client.
  #[napi(factory)]
  pub fn create(options: Option<HttpClientOptions>) -> Result<Self> {
    let opts = options.unwrap_or_default();
    let core_opts = ferridriver::http_client::HttpClientOptions {
      base_url: opts.base_url,
      extra_http_headers: opts
        .extra_http_headers
        .as_ref()
        .map(|h| {
          h.iter()
            .filter_map(|pair| {
              if pair.len() == 2 {
                Some((pair[0].clone(), pair[1].clone()))
              } else {
                None
              }
            })
            .collect()
        })
        .unwrap_or_default(),
      timeout: opts.timeout.map(|t| std::time::Duration::from_millis(t as u64)),
      ignore_https_errors: opts.ignore_https_errors.unwrap_or(false),
    };
    Ok(Self {
      inner: ferridriver::http_client::HttpClient::new(core_opts),
    })
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
          headers: Some(replayable_headers(req.inner.headers().into_iter().collect())),
          data: req.inner.post_data_buffer(),
          ..Default::default()
        }),
      ),
    };
    let opts = merge_over(base, options.map(|o| o.to_core()).transpose()?);
    let resp = self.inner.fetch(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Dispose the request context.
  #[napi]
  pub fn dispose(&self) {}
}
