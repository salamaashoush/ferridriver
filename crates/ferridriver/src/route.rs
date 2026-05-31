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
//! use ferridriver::url_matcher::UrlMatcher;
//!
//! page.route(UrlMatcher::glob("**/api/*")?, Arc::new(|route: Route| {
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
//! }), None).await?;
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

  /// Build a [`crate::network::Request`] view over the intercepted
  /// request. Mirrors Playwright's `route.request(): Request`
  /// (`client/network.ts`), where the returned object is the full
  /// `Request` API rather than the raw interception record. The
  /// resulting `Request` carries the intercepted URL, method, headers,
  /// post body, and resource type; it has no live response / timing /
  /// redirect chain because an intercepted-but-not-yet-continued
  /// request has not produced any of those yet.
  #[must_use]
  pub fn network_request(&self) -> crate::network::Request {
    crate::network::Request::new(crate::network::RequestInit {
      id: self.request.request_id.clone(),
      url: self.request.url.clone(),
      method: self.request.method.clone(),
      resource_type: self.request.resource_type.clone(),
      is_navigation_request: false,
      post_data: self.request.post_data.clone().map(String::into_bytes),
      headers: self.request.headers.clone(),
      frame_id: None,
      redirected_from: None,
      timing: None,
      raw_headers_fn: None,
    })
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

  /// Fall back to the next matching handler, applying the given
  /// overrides. Mirrors Playwright's `route.fallback(options?)`
  /// (`client/network.ts`): it records the fallback overrides and
  /// reports the route as not handled so a subsequent handler (or the
  /// default behaviour) takes over.
  ///
  /// ferridriver dispatches a single handler per matched route, so the
  /// "next handler" is the default continue: `fallback` resolves the
  /// route by continuing the request with the supplied overrides
  /// applied. With no overrides this is identical to letting the
  /// request proceed unmodified, which is exactly Playwright's
  /// `fallback()` end state once no further handler claims it.
  pub fn fallback(mut self, overrides: ContinueOverrides) {
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

/// A registered route: URL matcher + handler.
///
/// Matching delegates to [`crate::url_matcher::UrlMatcher::matches`]; equality
/// for `unroute` uses [`crate::url_matcher::UrlMatcher::equivalent`] so a
/// caller passing the same glob string later can retire the registration.
pub struct RegisteredRoute {
  /// Matcher that decides which URLs this route intercepts.
  pub matcher: crate::url_matcher::UrlMatcher,
  /// The handler function.
  pub handler: RouteHandler,
  /// Remaining number of times this handler may fire (Playwright `times`
  /// option). `None` means unlimited. Interior-mutable so the interception
  /// loop can decrement it without upgrading its read lock; a route whose
  /// counter has reached zero is skipped (and pruned opportunistically).
  pub remaining: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
}

impl RegisteredRoute {
  /// Build a route registration, optionally limited to `times` invocations.
  #[must_use]
  pub fn new(matcher: crate::url_matcher::UrlMatcher, handler: RouteHandler, times: Option<u32>) -> Self {
    Self {
      matcher,
      handler,
      remaining: times.map(|t| std::sync::Arc::new(std::sync::atomic::AtomicU32::new(t))),
    }
  }

  /// Whether this route may still fire (unlimited, or counter > 0).
  #[must_use]
  pub fn live(&self) -> bool {
    self
      .remaining
      .as_ref()
      .is_none_or(|c| c.load(std::sync::atomic::Ordering::Acquire) > 0)
  }
}

/// Select the first live route matching `url`, atomically consume one unit of
/// its `times` budget, and drop it once exhausted. Returns the handler to run,
/// or `None` when no live route matches. Shared by every backend's
/// interception loop so the `times` semantics are identical everywhere.
#[must_use]
pub fn take_matching_handler(routes: &mut Vec<RegisteredRoute>, url: &str) -> Option<RouteHandler> {
  let idx = routes.iter().position(|r| r.live() && r.matcher.matches(url))?;
  let handler = std::sync::Arc::clone(&routes[idx].handler);
  let exhausted = routes[idx].remaining.as_ref().is_some_and(|c| {
    c.fetch_update(
      std::sync::atomic::Ordering::AcqRel,
      std::sync::atomic::Ordering::Acquire,
      |n| n.checked_sub(1),
    )
    .map_or(true, |prev| prev <= 1)
  });
  if exhausted {
    routes.remove(idx);
  }
  Some(handler)
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

#[cfg(test)]
mod tests {
  use super::*;

  fn sample_request() -> InterceptedRequest {
    let mut headers = FxHashMap::default();
    headers.insert("x-from".to_string(), "test".to_string());
    InterceptedRequest {
      request_id: "req-1".to_string(),
      url: "https://example.com/api".to_string(),
      method: "POST".to_string(),
      headers,
      post_data: Some("hello".to_string()),
      resource_type: "Fetch".to_string(),
    }
  }

  #[tokio::test]
  async fn fallback_sends_continue_with_overrides() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let route = Route::new(sample_request(), tx);
    route.fallback(ContinueOverrides {
      method: Some("PUT".to_string()),
      ..Default::default()
    });
    match rx.await.expect("route action") {
      RouteAction::Continue(o) => assert_eq!(o.method.as_deref(), Some("PUT")),
      other => panic!("expected Continue, got {other:?}"),
    }
  }

  #[tokio::test]
  async fn fallback_without_overrides_sends_unmodified_continue() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let route = Route::new(sample_request(), tx);
    route.fallback(ContinueOverrides::default());
    match rx.await.expect("route action") {
      RouteAction::Continue(o) => {
        assert!(o.url.is_none() && o.method.is_none() && o.headers.is_none() && o.post_data.is_none());
      },
      other => panic!("expected Continue, got {other:?}"),
    }
  }

  #[test]
  fn network_request_carries_interception_fields() {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let route = Route::new(sample_request(), tx);
    let req = route.network_request();
    assert_eq!(req.url(), "https://example.com/api");
    assert_eq!(req.method(), "POST");
    assert_eq!(req.resource_type(), "Fetch");
    assert_eq!(req.post_data().as_deref(), Some("hello"));
    assert!(!req.is_navigation_request());
  }
}
