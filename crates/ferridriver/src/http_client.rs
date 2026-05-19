//! WHATWG-fetch-compatible HTTP client -- the runner-side request
//! stack, separate from the browser/page network. Backs both the
//! `fetch` global and the Playwright-style `request` binding.
//!
//! Provides `HttpClient` with `get`, `post`, `put`, `delete`, `patch`,
//! `head`, and generic `fetch`.
//!
//! Each method returns an `HttpResponse` with `status()`, `text()`, `json()`,
//! `headers()`, `ok()`, and `body()`.
//!
//! ```ignore
//! let ctx = HttpClient::new(HttpClientOptions {
//!     base_url: Some("https://api.example.com".into()),
//!     ..Default::default()
//! });
//! let resp = ctx.get("/users", None).await?;
//! assert!(resp.ok());
//! let users: Vec<User> = resp.json()?;
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustc_hash::FxHashMap;

/// Options for creating an `HttpClient`.
#[derive(Debug, Clone, Default)]
pub struct HttpClientOptions {
  /// Base URL prepended to relative paths (e.g., `"https://api.example.com"`).
  pub base_url: Option<String>,
  /// Default headers sent with every request.
  pub extra_http_headers: Vec<(String, String)>,
  /// Default timeout per request.
  pub timeout: Option<Duration>,
  /// Ignore HTTPS certificate errors.
  pub ignore_https_errors: bool,
}

/// Per-request options (overrides context defaults).
#[derive(Debug, Clone, Default)]
pub struct RequestOptions {
  /// Override HTTP method (normally set by the convenience method).
  pub method: Option<String>,
  /// Extra headers for this request.
  pub headers: Option<Vec<(String, String)>>,
  /// Raw request body.
  pub data: Option<Vec<u8>>,
  /// JSON request body (serialized automatically, sets Content-Type).
  pub json_data: Option<serde_json::Value>,
  /// URL-encoded form data.
  pub form: Option<Vec<(String, String)>>,
  /// Query string parameters.
  pub params: Option<Vec<(String, String)>>,
  /// Per-request timeout override.
  pub timeout: Option<Duration>,
  /// Fail with error on 4xx/5xx status codes.
  pub fail_on_status_code: Option<bool>,
  /// Per-request redirect cap: `Some(0)` does not follow redirects
  /// (the 3xx is returned as-is), `Some(n)` follows up to `n` then
  /// errors, `None` uses the client default.
  pub max_redirects: Option<u32>,
}

/// An HTTP response.
#[derive(Debug, Clone)]
pub struct HttpResponse {
  status_code: u16,
  status_text: String,
  response_url: String,
  response_headers: Vec<(String, String)>,
  body_bytes: bytes::Bytes,
}

impl HttpResponse {
  /// HTTP status code.
  pub fn status(&self) -> u16 {
    self.status_code
  }

  /// HTTP status text (e.g., "OK", "Not Found").
  pub fn status_text(&self) -> &str {
    &self.status_text
  }

  /// Final URL after redirects.
  pub fn url(&self) -> &str {
    &self.response_url
  }

  /// Whether the response status is 200-299.
  pub fn ok(&self) -> bool {
    (200..300).contains(&self.status_code)
  }

  /// Response headers as (name, value) pairs.
  pub fn headers(&self) -> &[(String, String)] {
    &self.response_headers
  }

  /// Get a specific header value by name (case-insensitive).
  pub fn header(&self, name: &str) -> Option<&str> {
    let lower = name.to_lowercase();
    self
      .response_headers
      .iter()
      .find(|(k, _)| k.to_lowercase() == lower)
      .map(|(_, v)| v.as_str())
  }

  /// Response body as UTF-8 string.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid UTF-8.
  pub fn text(&self) -> crate::error::Result<String> {
    String::from_utf8(self.body_bytes.to_vec())
      .map_err(|e| crate::error::FerriError::evaluation(format!("response body is not UTF-8: {e}")))
  }

  /// Parse response body as JSON.
  ///
  /// # Errors
  ///
  /// Returns an error if the body cannot be deserialized.
  pub fn json<T: serde::de::DeserializeOwned>(&self) -> crate::error::Result<T> {
    serde_json::from_slice(&self.body_bytes).map_err(Into::into)
  }

  /// Response body as a JSON value.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid JSON.
  pub fn json_value(&self) -> crate::error::Result<serde_json::Value> {
    self.json()
  }

  /// Raw response body bytes.
  pub fn body(&self) -> &[u8] {
    &self.body_bytes
  }

  /// Consume the response (Playwright compat, no-op in Rust since we own the bytes).
  pub fn dispose(self) {
    drop(self);
  }
}

/// A general HTTP client: all methods, JSON/form/multipart bodies,
/// query params, custom headers, timeouts, and cookie persistence via
/// reqwest's cookie jar. The one stack `fetch` and `request` share.
#[derive(Clone)]
pub struct HttpClient {
  client: reqwest::Client,
  base_url: Option<String>,
  extra_headers: Vec<(String, String)>,
  default_timeout: Duration,
  /// Shared cookie jar. reqwest pins the redirect policy on the
  /// `Client`, so a per-request `max_redirects` override needs a
  /// distinct `Client`; every such client is built against THIS jar so
  /// session cookies still persist across calls regardless of which
  /// redirect-policy client served a given request.
  jar: Arc<reqwest::cookie::Jar>,
  ignore_https_errors: bool,
  /// Lazily-built clients keyed by requested redirect limit (`0` =
  /// don't follow, `n` = follow up to `n`). The default-policy client
  /// is `self.client`; this only holds the per-override ones.
  redirect_clients: Arc<Mutex<FxHashMap<u32, reqwest::Client>>>,
}

/// Build a reqwest client sharing `jar` (so cookies persist across the
/// default and any per-redirect-limit clients). `max_redirects`:
/// `None` keeps reqwest's default policy, `Some(0)` does not follow
/// redirects, `Some(n)` follows up to `n` (exceeding errors).
fn build_client(
  jar: &Arc<reqwest::cookie::Jar>,
  ignore_https_errors: bool,
  max_redirects: Option<u32>,
) -> reqwest::Client {
  let mut builder = reqwest::Client::builder().cookie_provider(jar.clone());
  if let Some(max) = max_redirects {
    let policy = if max == 0 {
      reqwest::redirect::Policy::none()
    } else {
      reqwest::redirect::Policy::limited(max as usize)
    };
    builder = builder.redirect(policy);
  }
  if ignore_https_errors {
    builder = builder.danger_accept_invalid_certs(true);
  }
  builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

impl HttpClient {
  /// Create a new HTTP client.
  #[must_use]
  pub fn new(options: HttpClientOptions) -> Self {
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let client = build_client(&jar, options.ignore_https_errors, None);
    let default_timeout = options.timeout.unwrap_or(Duration::from_secs(30));

    Self {
      client,
      base_url: options.base_url,
      extra_headers: options.extra_http_headers,
      default_timeout,
      jar,
      ignore_https_errors: options.ignore_https_errors,
      redirect_clients: Arc::new(Mutex::new(FxHashMap::default())),
    }
  }

  /// The reqwest client to use for a request: the default-policy one,
  /// or — when the caller pinned `max_redirects` — a jar-sharing client
  /// built for exactly that limit (built once, then cached).
  fn client_for(&self, max_redirects: Option<u32>) -> reqwest::Client {
    let Some(max) = max_redirects else {
      return self.client.clone();
    };
    let mut cache = self
      .redirect_clients
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache
      .entry(max)
      .or_insert_with(|| build_client(&self.jar, self.ignore_https_errors, Some(max)))
      .clone()
  }

  /// Resolve a URL against the base URL.
  fn resolve_url(&self, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
      return url.to_string();
    }
    match &self.base_url {
      Some(base) => {
        let base = base.trim_end_matches('/');
        if url.starts_with('/') {
          format!("{base}{url}")
        } else {
          format!("{base}/{url}")
        }
      },
      None => url.to_string(),
    }
  }

  /// Send a GET request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn get(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("GET".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a POST request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn post(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("POST".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a PUT request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn put(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("PUT".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a DELETE request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn delete(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("DELETE".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a PATCH request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn patch(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("PATCH".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send a HEAD request.
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or status-code validation fails.
  pub async fn head(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    self
      .fetch(
        url,
        Some(RequestOptions {
          method: Some("HEAD".into()),
          ..options.unwrap_or_default()
        }),
      )
      .await
  }

  /// Send an HTTP request (generic method — all verbs delegate here).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or `fail_on_status_code` is set and the response is 4xx/5xx.
  pub async fn fetch(&self, url: &str, options: Option<RequestOptions>) -> crate::error::Result<HttpResponse> {
    let opts = options.unwrap_or_default();
    let method_str = opts.method.as_deref().unwrap_or("GET");
    let method: reqwest::Method = method_str
      .parse()
      .map_err(|_| format!("invalid HTTP method: {method_str}"))?;

    let resolved_url = self.resolve_url(url);
    let mut builder = self.client_for(opts.max_redirects).request(method, &resolved_url);

    // Apply default extra headers.
    for (k, v) in &self.extra_headers {
      builder = builder.header(k, v);
    }

    // Apply per-request headers.
    if let Some(headers) = &opts.headers {
      for (k, v) in headers {
        builder = builder.header(k, v);
      }
    }

    // Query parameters.
    if let Some(params) = &opts.params {
      builder = builder.query(params);
    }

    // Request body (mutually exclusive: data, json_data, form).
    if let Some(json) = &opts.json_data {
      builder = builder.json(json);
    } else if let Some(form) = &opts.form {
      builder = builder.form(form);
    } else if let Some(data) = &opts.data {
      builder = builder.body(data.clone());
    }

    // Timeout.
    let timeout = opts.timeout.unwrap_or(self.default_timeout);
    builder = builder.timeout(timeout);

    // Send.
    let response = builder
      .send()
      .await
      .map_err(|e| format!("request to {resolved_url} failed: {e}"))?;

    let status_code = response.status().as_u16();
    let status_text = response.status().canonical_reason().unwrap_or("Unknown").to_string();
    let response_url = response.url().to_string();
    let response_headers: Vec<(String, String)> = response
      .headers()
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
      .collect();

    let body_bytes = response.bytes().await.map_err(|e| format!("read response body: {e}"))?;

    let api_response = HttpResponse {
      status_code,
      status_text,
      response_url,
      response_headers,
      body_bytes,
    };

    // Fail on status code if requested.
    if opts.fail_on_status_code.unwrap_or(false) && !api_response.ok() {
      return Err(crate::error::FerriError::Backend(format!(
        "{} {resolved_url} failed: {} {}",
        method_str,
        api_response.status(),
        api_response.status_text()
      )));
    }

    Ok(api_response)
  }

  /// Dispose the request context (Playwright compat).
  pub fn dispose(self) {
    drop(self);
  }
}
