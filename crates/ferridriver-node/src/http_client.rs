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

/// Per-request options.
#[napi(object)]
#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
  /// Extra headers for this request.
  pub headers: Option<Vec<Vec<String>>>,
  /// JSON request body (auto-serializes, sets Content-Type).
  pub data: Option<serde_json::Value>,
  /// URL-encoded form data as `[[key, value], ...]`.
  pub form: Option<Vec<Vec<String>>>,
  /// Query string parameters as `[[key, value], ...]`.
  pub params: Option<Vec<Vec<String>>>,
  /// Timeout in milliseconds.
  pub timeout: Option<f64>,
  /// Fail with error on 4xx/5xx.
  pub fail_on_status_code: Option<bool>,
  /// Max redirects.
  pub max_redirects: Option<i32>,
}

impl FetchOptions {
  fn to_core(&self) -> ferridriver::http_client::RequestOptions {
    ferridriver::http_client::RequestOptions {
      method: None,
      headers: self.headers.as_ref().map(|h| {
        h.iter()
          .filter_map(|pair| {
            if pair.len() == 2 {
              Some((pair[0].clone(), pair[1].clone()))
            } else {
              None
            }
          })
          .collect()
      }),
      json_data: self.data.clone(),
      data: None,
      form: self.form.as_ref().map(|f| {
        f.iter()
          .filter_map(|pair| {
            if pair.len() == 2 {
              Some((pair[0].clone(), pair[1].clone()))
            } else {
              None
            }
          })
          .collect()
      }),
      params: self.params.as_ref().map(|p| {
        p.iter()
          .filter_map(|pair| {
            if pair.len() == 2 {
              Some((pair[0].clone(), pair[1].clone()))
            } else {
              None
            }
          })
          .collect()
      }),
      timeout: self.timeout.map(|t| std::time::Duration::from_millis(t as u64)),
      fail_on_status_code: self.fail_on_status_code,
      max_redirects: self.max_redirects.map(|m| m as u32),
      // The Node binding is the trusted Playwright-in-Rust surface, not
      // the script sandbox — no network guard is imposed here.
      net_guard: None,
    }
  }
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
    let opts = options.map(|o| o.to_core());
    let resp = self.inner.get(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a POST request.
  #[napi]
  pub async fn post(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core());
    let resp = self.inner.post(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a PUT request.
  #[napi]
  pub async fn put(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core());
    let resp = self.inner.put(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a DELETE request.
  #[napi]
  pub async fn delete(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core());
    let resp = self.inner.delete(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a PATCH request.
  #[napi]
  pub async fn patch(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core());
    let resp = self.inner.patch(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a HEAD request.
  #[napi]
  pub async fn head(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core());
    let resp = self.inner.head(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Send a generic HTTP request.
  #[napi]
  pub async fn fetch(&self, url: String, options: Option<FetchOptions>) -> Result<HttpResponse> {
    let opts = options.map(|o| o.to_core());
    let resp = self.inner.fetch(&url, opts).await.into_napi()?;
    Ok(HttpResponse { inner: resp })
  }

  /// Dispose the request context.
  #[napi]
  pub fn dispose(&self) {}
}
