//! Network request interception -- `page.route()` / `page.unroute()`.
//!
//! Mirrors Playwright's Route API for intercepting, mocking, and modifying
//! network requests. Uses CDP Fetch domain on Chrome backends.
//!
//! The handler receives a `Route` object and must call exactly one of
//! `fulfill()`, `continue_route()`, or `abort()`. If the handler drops
//! the `Route` without calling any method, the request is continued
//! with no modifications (fail-open).
//!
//! ```ignore
//! page.route("**/api/*", Arc::new(|route: Route| {
//!     if route.request().url.contains("block-me") {
//!         route.abort("blockedbyclient");
//!     } else {
//!         route.fulfill(FulfillResponse {
//!             status: 200,
//!             body: b"mocked".to_vec(),
//!             content_type: Some("text/plain".into()),
//!             ..Default::default()
//!         });
//!     }
//! })).await?;
//! ```

use rustc_hash::FxHashMap;

/// How to respond to an intercepted request.
#[derive(Debug, Clone)]
pub enum RouteAction {
  /// Continue the request, optionally modifying URL/method/headers/postData.
  Continue(ContinueOverrides),
  /// Fulfill with a custom response (mock).
  Fulfill(FulfillResponse),
  /// Abort the request with an error reason.
  Abort(String),
}

/// Overrides when continuing an intercepted request.
#[derive(Debug, Clone, Default)]
pub struct ContinueOverrides {
  /// Override the URL (must keep same protocol).
  pub url: Option<String>,
  /// Override the HTTP method.
  pub method: Option<String>,
  /// Override request headers.
  pub headers: Option<Vec<(String, String)>>,
  /// Override the request body (raw bytes, will be base64-encoded for CDP).
  pub post_data: Option<Vec<u8>>,
}

/// A mocked response for fulfilling an intercepted request.
#[derive(Debug, Clone)]
pub struct FulfillResponse {
  /// HTTP status code (default: 200).
  pub status: i32,
  /// Response headers.
  pub headers: Vec<(String, String)>,
  /// Response body.
  pub body: Vec<u8>,
  /// Content type (convenience, added to headers if set).
  pub content_type: Option<String>,
}

impl Default for FulfillResponse {
  fn default() -> Self {
    Self {
      status: 200,
      headers: vec![],
      body: vec![],
      content_type: None,
    }
  }
}

/// An intercepted request with metadata.
#[derive(Debug, Clone)]
pub struct InterceptedRequest {
  /// CDP Fetch request ID (needed for fulfill/continue/abort).
  pub request_id: String,
  /// Request URL.
  pub url: String,
  /// HTTP method.
  pub method: String,
  /// Request headers.
  pub headers: FxHashMap<String, String>,
  /// POST body (if any).
  pub post_data: Option<String>,
  /// Resource type (Document, Script, Stylesheet, Image, etc.).
  pub resource_type: String,
}

/// A paused network request. The handler must call exactly one of
/// `fulfill()`, `continue_route()`, or `abort()` to resume the request.
///
/// If dropped without calling any method, the request is continued
/// with no modifications (fail-open).
pub struct Route {
  request: InterceptedRequest,
  action_tx: Option<tokio::sync::oneshot::Sender<RouteAction>>,
}

impl Route {
  /// Create a new Route with its response channel.
  #[must_use]
  pub fn new(request: InterceptedRequest, action_tx: tokio::sync::oneshot::Sender<RouteAction>) -> Self {
    Self {
      request,
      action_tx: Some(action_tx),
    }
  }

  /// The intercepted request.
  #[must_use]
  pub fn request(&self) -> &InterceptedRequest {
    &self.request
  }

  /// Fulfill with a custom response (mock).
  pub fn fulfill(mut self, response: FulfillResponse) {
    if let Some(tx) = self.action_tx.take() {
      let _ = tx.send(RouteAction::Fulfill(response));
    }
  }

  /// Continue the request, optionally with modifications.
  pub fn continue_route(mut self, overrides: ContinueOverrides) {
    if let Some(tx) = self.action_tx.take() {
      let _ = tx.send(RouteAction::Continue(overrides));
    }
  }

  /// Abort the request with an error reason.
  pub fn abort(mut self, reason: &str) {
    if let Some(tx) = self.action_tx.take() {
      let _ = tx.send(RouteAction::Abort(reason.to_string()));
    }
  }
}

impl Drop for Route {
  fn drop(&mut self) {
    // Fail-open: if the handler didn't call fulfill/continue/abort,
    // continue the request with no modifications.
    if let Some(tx) = self.action_tx.take() {
      let _ = tx.send(RouteAction::Continue(ContinueOverrides::default()));
    }
  }
}

/// Route handler function type.
/// Receives a `Route` object; must call one of `fulfill()`, `continue_route()`, or `abort()`.
/// Must be Send + Sync since it's called from async tasks.
pub type RouteHandler = std::sync::Arc<dyn Fn(Route) + Send + Sync>;

/// A registered route with URL pattern and handler.
pub struct RegisteredRoute {
  /// URL pattern (glob converted to regex).
  pub pattern: regex::Regex,
  /// Original pattern string (for display/unroute matching).
  pub pattern_str: String,
  /// The handler function.
  pub handler: RouteHandler,
}

/// Convert a glob URL pattern to a regex.
/// Supports: `*` (any chars except /), `**` (any chars including /), `?` (single char).
///
/// # Errors
///
/// Returns an error if the resulting regex pattern is invalid.
pub fn glob_to_regex(glob: &str) -> Result<regex::Regex, String> {
  let mut regex = String::with_capacity(glob.len() * 2);
  regex.push('^');
  let mut chars = glob.chars().peekable();
  while let Some(c) = chars.next() {
    match c {
      '*' => {
        if chars.peek() == Some(&'*') {
          chars.next();
          regex.push_str(".*"); // ** = match everything including /
        } else {
          regex.push_str("[^/]*"); // * = match everything except /
        }
      },
      '?' => regex.push('.'),
      '.' | '+' | '^' | '$' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' => {
        regex.push('\\');
        regex.push(c);
      },
      _ => regex.push(c),
    }
  }
  regex.push('$');
  regex::Regex::new(&regex).map_err(|e| format!("Invalid route pattern '{glob}': {e}"))
}

/// HTTP status text for common status codes.
#[must_use]
pub fn status_text(code: i32) -> &'static str {
  match code {
    201 => "Created",
    204 => "No Content",
    301 => "Moved Permanently",
    302 => "Found",
    304 => "Not Modified",
    400 => "Bad Request",
    401 => "Unauthorized",
    403 => "Forbidden",
    404 => "Not Found",
    405 => "Method Not Allowed",
    500 => "Internal Server Error",
    502 => "Bad Gateway",
    503 => "Service Unavailable",
    // 200 and all unknown codes default to "OK"
    _ => "OK",
  }
}
