//! Network request interception -- `page.route()` / `page.unroute()`.
//!
//! Mirrors Playwright's Route API for intercepting, mocking, and modifying
//! network requests. Uses CDP Fetch domain on Chrome backends.

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

/// Route handler function type.
/// Takes the intercepted request, returns what action to take.
/// Must be Send + Sync since it's called from async tasks.
pub type RouteHandler = std::sync::Arc<dyn Fn(&InterceptedRequest) -> RouteAction + Send + Sync>;

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
            }
            '?' => regex.push('.'),
            '.' | '+' | '^' | '$' | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '\\' => {
                regex.push('\\');
                regex.push(c);
            }
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
