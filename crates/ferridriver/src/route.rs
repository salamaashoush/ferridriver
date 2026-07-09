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
  /// Pass to the next matching handler, applying the given overrides to
  /// the request the next handler observes. Mirrors Playwright's
  /// `route.fallback(options?)`: consumed by [`run_route_chain`], never
  /// delivered to a backend resolution path.
  Fallback(ContinueOverrides),
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
  /// (`client/network.ts`): the overrides mutate the request the next
  /// matching handler observes (`_applyFallbackOverrides`), and when no
  /// further handler claims the route the request continues with all
  /// accumulated overrides applied.
  pub fn fallback(mut self, overrides: ContinueOverrides) {
    if let Some(tx) = self.action_tx.take() {
      let _ = tx.send(RouteAction::Fallback(overrides));
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

/// Which registration surface a route came from. Playwright keeps page
/// routes (`page._routes`) and context routes (`context._routes`) in
/// separate lists — page routes are consulted first, and
/// `page.unrouteAll` / `context.unrouteAll` each clear only their own
/// scope. ferridriver stores both in the page's single route list, so
/// the scope tag preserves those semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteScope {
  /// Registered via `page.route` / `page.routeFromHAR`.
  Page,
  /// Registered via `context.route` / `context.routeFromHAR` and fanned
  /// out to every page of the context.
  Context,
}

/// A registered route: URL matcher + handler.
///
/// Matching delegates to [`crate::url_matcher::UrlMatcher::matches`]; equality
/// for `unroute` uses [`crate::url_matcher::UrlMatcher::equivalent`] so a
/// caller passing the same glob string later can retire the registration.
///
/// Cloning shares the `times` budget: a context-scoped route cloned onto
/// several pages consumes one context-wide counter, matching Playwright
/// where the context holds a single `RouteHandler` for all its pages.
#[derive(Clone)]
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
  /// Whether this registration is page- or context-scoped.
  pub scope: RouteScope,
}

impl RegisteredRoute {
  /// Build a page-scoped registration, optionally limited to `times`
  /// invocations.
  #[must_use]
  pub fn new(matcher: crate::url_matcher::UrlMatcher, handler: RouteHandler, times: Option<u32>) -> Self {
    Self::scoped(matcher, handler, times, RouteScope::Page)
  }

  /// Build a context-scoped registration. Clones of the returned value
  /// share one `times` budget across every page they are installed on.
  #[must_use]
  pub fn context_scoped(matcher: crate::url_matcher::UrlMatcher, handler: RouteHandler, times: Option<u32>) -> Self {
    Self::scoped(matcher, handler, times, RouteScope::Context)
  }

  fn scoped(
    matcher: crate::url_matcher::UrlMatcher,
    handler: RouteHandler,
    times: Option<u32>,
    scope: RouteScope,
  ) -> Self {
    Self {
      matcher,
      handler,
      remaining: times.map(|t| std::sync::Arc::new(std::sync::atomic::AtomicU32::new(t))),
      scope,
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

  /// Stable identity of the underlying handler closure — used by the
  /// chain driver to skip handlers that already ran for this request.
  fn handler_id(&self) -> usize {
    std::sync::Arc::as_ptr(&self.handler).cast::<()>() as usize
  }
}

/// Whether any live route matches `url` — cheap pre-check the backend
/// interception loops run before parsing the full request payload.
#[must_use]
pub fn any_matching_route(routes: &[RegisteredRoute], url: &str) -> bool {
  routes.iter().any(|r| r.live() && r.matcher.matches(url))
}

/// Select the next live route matching `url` in Playwright precedence
/// order — page-scoped registrations first, context-scoped after, newest
/// first within each scope (Playwright `unshift`s new handlers and scans
/// in order; our lists append, so precedence scans in reverse) — skipping
/// handlers already consulted for this request. Consumes one unit of the
/// selected route's `times` budget (Playwright removes an expiring
/// handler *before* invoking it) and prunes it once exhausted.
fn take_next_handler(routes: &mut Vec<RegisteredRoute>, url: &str, tried: &[usize]) -> Option<(RouteHandler, usize)> {
  let pick = |scope: RouteScope, routes: &Vec<RegisteredRoute>| {
    routes
      .iter()
      .enumerate()
      .rev()
      .find(|(_, r)| r.scope == scope && r.live() && !tried.contains(&r.handler_id()) && r.matcher.matches(url))
      .map(|(i, _)| i)
  };
  let idx = pick(RouteScope::Page, routes).or_else(|| pick(RouteScope::Context, routes))?;
  let handler = std::sync::Arc::clone(&routes[idx].handler);
  let id = routes[idx].handler_id();
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
  Some((handler, id))
}

/// Layer `top` over `base`: fields set by `top` win, unset fields keep
/// the accumulated fallback value. Mirrors Playwright's
/// `route.continue()` merging with `_fallbackOverrides`.
fn merge_overrides(base: &ContinueOverrides, top: ContinueOverrides) -> ContinueOverrides {
  ContinueOverrides {
    url: top.url.or_else(|| base.url.clone()),
    method: top.method.or_else(|| base.method.clone()),
    headers: top.headers.or_else(|| base.headers.clone()),
    post_data: top.post_data.or_else(|| base.post_data.clone()),
  }
}

/// Apply fallback overrides to the request view the next handler sees.
fn apply_overrides(request: &mut InterceptedRequest, overrides: &ContinueOverrides) {
  if let Some(url) = &overrides.url {
    request.url.clone_from(url);
  }
  if let Some(method) = &overrides.method {
    request.method.clone_from(method);
  }
  if let Some(headers) = &overrides.headers {
    request.headers = headers.iter().cloned().collect();
  }
  if let Some(body) = &overrides.post_data {
    request.post_data = Some(String::from_utf8_lossy(body).into_owned());
  }
}

/// Drive the full handler chain for one intercepted request and return
/// the terminal action to execute against the wire. Mirrors Playwright's
/// `Page._onRoute` → `BrowserContext._onRoute` walk:
///
/// * handlers run in precedence order (page scope before context scope,
///   newest first within each);
/// * a handler's `times` budget is consumed when it is invoked, even if
///   it falls back;
/// * `route.fallback(overrides)` mutates the request the next handler
///   observes (including re-matching against an overridden URL) and
///   accumulates into the final continue;
/// * when no handler claims the request, it continues with all
///   accumulated fallback overrides applied;
/// * a terminal `continue` merges its own overrides over the accumulated
///   fallback overrides.
///
/// Shared by every backend's interception loop so precedence, `times`,
/// and fallback semantics are identical everywhere.
pub async fn run_route_chain(
  routes: &tokio::sync::RwLock<Vec<RegisteredRoute>>,
  mut request: InterceptedRequest,
) -> RouteAction {
  let mut accumulated = ContinueOverrides::default();
  let mut tried: Vec<usize> = Vec::new();
  loop {
    let picked = {
      let mut guard = routes.write().await;
      take_next_handler(&mut guard, &request.url, &tried)
    };
    let Some((handler, id)) = picked else {
      return RouteAction::Continue(accumulated);
    };
    tried.push(id);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let route = Route::new(request.clone(), tx);
    handler(route);
    let action = rx.await.unwrap_or(RouteAction::Continue(ContinueOverrides::default()));
    match action {
      RouteAction::Fallback(overrides) => {
        apply_overrides(&mut request, &overrides);
        accumulated = merge_overrides(&accumulated, overrides);
      },
      RouteAction::Continue(overrides) => {
        return RouteAction::Continue(merge_overrides(&accumulated, overrides));
      },
      terminal => return terminal,
    }
  }
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
  async fn fallback_sends_fallback_action_with_overrides() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let route = Route::new(sample_request(), tx);
    route.fallback(ContinueOverrides {
      method: Some("PUT".to_string()),
      ..Default::default()
    });
    match rx.await.expect("route action") {
      RouteAction::Fallback(o) => assert_eq!(o.method.as_deref(), Some("PUT")),
      other => panic!("expected Fallback, got {other:?}"),
    }
  }

  fn any_matcher() -> crate::url_matcher::UrlMatcher {
    crate::url_matcher::UrlMatcher::any()
  }

  fn fulfill_with_body(body: &'static str) -> RouteHandler {
    std::sync::Arc::new(move |route: Route| {
      route.fulfill(FulfillResponse {
        body: body.as_bytes().to_vec(),
        ..Default::default()
      });
    })
  }

  fn fulfilled_body(action: &RouteAction) -> &str {
    match action {
      RouteAction::Fulfill(resp) => std::str::from_utf8(&resp.body).expect("utf8 body"),
      other => panic!("expected Fulfill, got {other:?}"),
    }
  }

  #[tokio::test]
  async fn chain_picks_newest_registration_first() {
    let routes = tokio::sync::RwLock::new(vec![
      RegisteredRoute::new(any_matcher(), fulfill_with_body("old"), None),
      RegisteredRoute::new(any_matcher(), fulfill_with_body("new"), None),
    ]);
    let action = run_route_chain(&routes, sample_request()).await;
    assert_eq!(fulfilled_body(&action), "new");
  }

  #[tokio::test]
  async fn chain_prefers_page_scope_over_newer_context_scope() {
    let routes = tokio::sync::RwLock::new(vec![
      RegisteredRoute::new(any_matcher(), fulfill_with_body("page"), None),
      RegisteredRoute::context_scoped(any_matcher(), fulfill_with_body("context"), None),
    ]);
    let action = run_route_chain(&routes, sample_request()).await;
    assert_eq!(fulfilled_body(&action), "page");
  }

  #[tokio::test]
  async fn fallback_reaches_next_handler_with_overridden_request() {
    let seen_method = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let seen = std::sync::Arc::clone(&seen_method);
    let older: RouteHandler = std::sync::Arc::new(move |route: Route| {
      *seen.lock().expect("lock") = route.request().method.clone();
      route.fulfill(FulfillResponse::default());
    });
    let newer: RouteHandler = std::sync::Arc::new(|route: Route| {
      route.fallback(ContinueOverrides {
        method: Some("PATCH".to_string()),
        ..Default::default()
      });
    });
    let routes = tokio::sync::RwLock::new(vec![
      RegisteredRoute::new(any_matcher(), older, None),
      RegisteredRoute::new(any_matcher(), newer, None),
    ]);
    let action = run_route_chain(&routes, sample_request()).await;
    assert!(matches!(action, RouteAction::Fulfill(_)));
    assert_eq!(*seen_method.lock().expect("lock"), "PATCH");
  }

  #[tokio::test]
  async fn unclaimed_chain_continues_with_accumulated_overrides() {
    let only: RouteHandler = std::sync::Arc::new(|route: Route| {
      route.fallback(ContinueOverrides {
        method: Some("PUT".to_string()),
        ..Default::default()
      });
    });
    let routes = tokio::sync::RwLock::new(vec![RegisteredRoute::new(any_matcher(), only, None)]);
    let action = run_route_chain(&routes, sample_request()).await;
    match action {
      RouteAction::Continue(o) => assert_eq!(o.method.as_deref(), Some("PUT")),
      other => panic!("expected Continue, got {other:?}"),
    }
  }

  #[tokio::test]
  async fn terminal_continue_merges_over_accumulated_fallback_overrides() {
    let older: RouteHandler = std::sync::Arc::new(|route: Route| {
      route.continue_route(ContinueOverrides {
        headers: Some(vec![("x-late".to_string(), "1".to_string())]),
        ..Default::default()
      });
    });
    let newer: RouteHandler = std::sync::Arc::new(|route: Route| {
      route.fallback(ContinueOverrides {
        method: Some("PUT".to_string()),
        ..Default::default()
      });
    });
    let routes = tokio::sync::RwLock::new(vec![
      RegisteredRoute::new(any_matcher(), older, None),
      RegisteredRoute::new(any_matcher(), newer, None),
    ]);
    let action = run_route_chain(&routes, sample_request()).await;
    match action {
      RouteAction::Continue(o) => {
        assert_eq!(o.method.as_deref(), Some("PUT"));
        assert_eq!(
          o.headers.as_deref(),
          Some(&[("x-late".to_string(), "1".to_string())][..])
        );
      },
      other => panic!("expected Continue, got {other:?}"),
    }
  }

  #[tokio::test]
  async fn times_budget_consumed_even_when_handler_falls_back() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let calls2 = std::sync::Arc::clone(&calls);
    let limited: RouteHandler = std::sync::Arc::new(move |route: Route| {
      calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      route.fallback(ContinueOverrides::default());
    });
    let routes = tokio::sync::RwLock::new(vec![RegisteredRoute::new(any_matcher(), limited, Some(1))]);
    let first = run_route_chain(&routes, sample_request()).await;
    assert!(matches!(first, RouteAction::Continue(_)));
    let second = run_route_chain(&routes, sample_request()).await;
    assert!(matches!(second, RouteAction::Continue(_)));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(routes.read().await.is_empty(), "exhausted route should be pruned");
  }

  #[tokio::test]
  async fn cloned_context_route_shares_times_budget_across_lists() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let calls2 = std::sync::Arc::clone(&calls);
    let handler: RouteHandler = std::sync::Arc::new(move |route: Route| {
      calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      route.fulfill(FulfillResponse::default());
    });
    let shared = RegisteredRoute::context_scoped(any_matcher(), handler, Some(1));
    let page_a = tokio::sync::RwLock::new(vec![shared.clone()]);
    let page_b = tokio::sync::RwLock::new(vec![shared]);
    let first = run_route_chain(&page_a, sample_request()).await;
    assert!(matches!(first, RouteAction::Fulfill(_)));
    let second = run_route_chain(&page_b, sample_request()).await;
    assert!(
      matches!(second, RouteAction::Continue(_)),
      "budget spent on page A must exhaust the clone on page B"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
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
