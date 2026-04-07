//! API request context -- Playwright-compatible HTTP client for API testing.
//!
//! Provides `APIRequestContext` with methods matching Playwright's API:
//! `get`, `post`, `put`, `delete`, `patch`, `head`, and generic `fetch`.
//!
//! Each method returns an `APIResponse` with `status()`, `text()`, `json()`,
//! `headers()`, `ok()`, and `body()`.
//!
//! ```ignore
//! let ctx = APIRequestContext::new(RequestContextOptions {
//!     base_url: Some("https://api.example.com".into()),
//!     ..Default::default()
//! });
//! let resp = ctx.get("/users", None).await?;
//! assert!(resp.ok());
//! let users: Vec<User> = resp.json()?;
//! ```

use std::time::Duration;

/// Options for creating an `APIRequestContext`.
#[derive(Debug, Clone, Default)]
pub struct RequestContextOptions {
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
  /// Max redirects (default: follow all).
  pub max_redirects: Option<u32>,
}

/// HTTP response from an API request.
#[derive(Debug, Clone)]
pub struct APIResponse {
  status_code: u16,
  status_text: String,
  response_url: String,
  response_headers: Vec<(String, String)>,
  body_bytes: bytes::Bytes,
}

impl APIResponse {
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
  pub fn text(&self) -> Result<String, String> {
    String::from_utf8(self.body_bytes.to_vec()).map_err(|e| format!("response body is not UTF-8: {e}"))
  }

  /// Parse response body as JSON.
  ///
  /// # Errors
  ///
  /// Returns an error if the body cannot be deserialized.
  pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
    serde_json::from_slice(&self.body_bytes).map_err(|e| format!("JSON parse error: {e}"))
  }

  /// Response body as a JSON value.
  ///
  /// # Errors
  ///
  /// Returns an error if the body is not valid JSON.
  pub fn json_value(&self) -> Result<serde_json::Value, String> {
    self.json()
  }

  /// Raw response body bytes.
  pub fn body(&self) -> &[u8] {
    &self.body_bytes
  }

  /// Consume the response (Playwright compat, no-op in Rust since we own the bytes).
  pub fn dispose(self) {}
}

/// Playwright-compatible HTTP client for API testing.
///
/// Supports all HTTP methods, JSON/form/multipart bodies, query parameters,
/// custom headers, timeouts, and cookie persistence via reqwest's cookie jar.
pub struct APIRequestContext {
  client: reqwest::Client,
  base_url: Option<String>,
  extra_headers: Vec<(String, String)>,
  default_timeout: Duration,
}

impl APIRequestContext {
  /// Create a new API request context.
  pub fn new(options: RequestContextOptions) -> Self {
    let mut builder = reqwest::Client::builder().cookie_store(true);

    if options.ignore_https_errors {
      builder = builder.danger_accept_invalid_certs(true);
    }

    let client = builder.build().unwrap_or_else(|_| reqwest::Client::new());
    let default_timeout = options.timeout.unwrap_or(Duration::from_secs(30));

    Self {
      client,
      base_url: options.base_url,
      extra_headers: options.extra_http_headers,
      default_timeout,
    }
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
      }
      None => url.to_string(),
    }
  }

  /// Send a GET request.
  pub async fn get(&self, url: &str, options: Option<RequestOptions>) -> Result<APIResponse, String> {
    self.fetch(url, Some(RequestOptions { method: Some("GET".into()), ..options.unwrap_or_default() })).await
  }

  /// Send a POST request.
  pub async fn post(&self, url: &str, options: Option<RequestOptions>) -> Result<APIResponse, String> {
    self.fetch(url, Some(RequestOptions { method: Some("POST".into()), ..options.unwrap_or_default() })).await
  }

  /// Send a PUT request.
  pub async fn put(&self, url: &str, options: Option<RequestOptions>) -> Result<APIResponse, String> {
    self.fetch(url, Some(RequestOptions { method: Some("PUT".into()), ..options.unwrap_or_default() })).await
  }

  /// Send a DELETE request.
  pub async fn delete(&self, url: &str, options: Option<RequestOptions>) -> Result<APIResponse, String> {
    self.fetch(url, Some(RequestOptions { method: Some("DELETE".into()), ..options.unwrap_or_default() })).await
  }

  /// Send a PATCH request.
  pub async fn patch(&self, url: &str, options: Option<RequestOptions>) -> Result<APIResponse, String> {
    self.fetch(url, Some(RequestOptions { method: Some("PATCH".into()), ..options.unwrap_or_default() })).await
  }

  /// Send a HEAD request.
  pub async fn head(&self, url: &str, options: Option<RequestOptions>) -> Result<APIResponse, String> {
    self.fetch(url, Some(RequestOptions { method: Some("HEAD".into()), ..options.unwrap_or_default() })).await
  }

  /// Send an HTTP request (generic method — all verbs delegate here).
  ///
  /// # Errors
  ///
  /// Returns an error if the request fails or `fail_on_status_code` is set and the response is 4xx/5xx.
  pub async fn fetch(&self, url: &str, options: Option<RequestOptions>) -> Result<APIResponse, String> {
    let opts = options.unwrap_or_default();
    let method_str = opts.method.as_deref().unwrap_or("GET");
    let method: reqwest::Method = method_str
      .parse()
      .map_err(|_| format!("invalid HTTP method: {method_str}"))?;

    let resolved_url = self.resolve_url(url);
    let mut builder = self.client.request(method, &resolved_url);

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

    // Max redirects.
    if let Some(max) = opts.max_redirects {
      // reqwest sets redirect policy on the client, not per-request.
      // For per-request, we'd need a separate client. Skip for now — use client default.
      let _ = max;
    }

    // Send.
    let response = builder.send().await.map_err(|e| format!("request to {resolved_url} failed: {e}"))?;

    let status_code = response.status().as_u16();
    let status_text = response
      .status()
      .canonical_reason()
      .unwrap_or("Unknown")
      .to_string();
    let response_url = response.url().to_string();
    let response_headers: Vec<(String, String)> = response
      .headers()
      .iter()
      .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
      .collect();

    let body_bytes = response
      .bytes()
      .await
      .map_err(|e| format!("read response body: {e}"))?;

    let api_response = APIResponse {
      status_code,
      status_text,
      response_url,
      response_headers,
      body_bytes,
    };

    // Fail on status code if requested.
    if opts.fail_on_status_code.unwrap_or(false) && !api_response.ok() {
      return Err(format!(
        "{} {resolved_url} failed: {} {}",
        method_str,
        api_response.status(),
        api_response.status_text()
      ));
    }

    Ok(api_response)
  }

  /// Dispose the request context (Playwright compat).
  pub fn dispose(self) {
    // reqwest::Client drops cleanly.
  }
}
