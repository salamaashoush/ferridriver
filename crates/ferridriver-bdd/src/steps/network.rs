//! Network interception/mocking step definitions.
//!
//! Uses the `page.route()` / `page.unroute()` API to intercept, mock,
//! and block network requests during BDD scenarios.

use std::sync::{Arc, Mutex};

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver::route::{ContinueOverrides, FulfillResponse, InterceptedRequest};
use ferridriver::url_matcher::UrlMatcher;
use ferridriver_bdd_macros::{given, then, when};

/// Thread-safe log of intercepted requests stored in world state.
#[derive(Clone, Default)]
pub struct InterceptedRequests(Arc<Mutex<Vec<InterceptedRequest>>>);

impl InterceptedRequests {
  fn push(&self, req: InterceptedRequest) {
    if let Ok(mut log) = self.0.lock() {
      log.push(req);
    }
  }

  fn count_matching(&self, pattern: &str) -> usize {
    let Ok(log) = self.0.lock() else {
      return 0;
    };
    log.iter().filter(|r| r.url.contains(pattern)).count()
  }
}

/// Get or initialize the intercepted-requests tracker in world state.
fn intercepted_requests(world: &mut BrowserWorld) -> InterceptedRequests {
  if let Some(existing) = world.get_state::<InterceptedRequests>() {
    return existing.clone();
  }
  let tracker = InterceptedRequests::default();
  world.set_state(tracker.clone());
  tracker
}

#[given("I mock requests to {string} with status {int} and body {string}")]
async fn mock_with_status_and_body(world: &mut BrowserWorld, pattern: String, status: i64, body: String) {
  let status = i32::try_from(status).map_err(|_| StepError::from(format!("invalid status code: {status}")))?;
  let body_bytes = body.into_bytes();
  let matcher =
    UrlMatcher::glob(&pattern).map_err(|e| StepError::from(format!("invalid url pattern \"{pattern}\": {e}")))?;
  world
    .page()
    .route(
      matcher,
      Arc::new(move |route| {
        route.fulfill(FulfillResponse {
          status,
          body: body_bytes.clone(),
          ..FulfillResponse::default()
        });
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("mock requests to \"{pattern}\": {e}")))?;
}

#[given("I mock requests to {string} with JSON {string}")]
async fn mock_with_json(world: &mut BrowserWorld, pattern: String, json_body: String) {
  let body_bytes = json_body.into_bytes();
  let matcher =
    UrlMatcher::glob(&pattern).map_err(|e| StepError::from(format!("invalid url pattern \"{pattern}\": {e}")))?;
  world
    .page()
    .route(
      matcher,
      Arc::new(move |route| {
        route.fulfill(FulfillResponse {
          status: 200,
          body: body_bytes.clone(),
          content_type: Some("application/json".to_string()),
          ..FulfillResponse::default()
        });
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("mock JSON requests to \"{pattern}\": {e}")))?;
}

#[given("I mock requests to {string} with fixture {string}")]
async fn mock_with_fixture(world: &mut BrowserWorld, pattern: String, fixture_path: String) {
  let path = world.resolve_fixture_path(&fixture_path);
  let body = std::fs::read(&path).map_err(|e| StepError::from(format!("read fixture {}: {e}", path.display())))?;

  // Infer content type from extension.
  let content_type = match path.extension().and_then(|e| e.to_str()) {
    Some("json") => "application/json",
    Some("html") | Some("htm") => "text/html",
    Some("xml") => "application/xml",
    Some("txt") => "text/plain",
    Some("css") => "text/css",
    Some("js") => "application/javascript",
    _ => "application/octet-stream",
  }
  .to_string();

  let matcher =
    UrlMatcher::glob(&pattern).map_err(|e| StepError::from(format!("invalid url pattern \"{pattern}\": {e}")))?;
  world
    .page()
    .route(
      matcher,
      Arc::new(move |route| {
        route.fulfill(FulfillResponse {
          status: 200,
          body: body.clone(),
          content_type: Some(content_type.clone()),
          ..FulfillResponse::default()
        });
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("mock with fixture \"{}\": {e}", fixture_path)))?;
}

#[given("I mock requests to {string} with fixture {string} and status {int}")]
async fn mock_with_fixture_and_status(world: &mut BrowserWorld, pattern: String, fixture_path: String, status: i64) {
  let status = i32::try_from(status).map_err(|_| StepError::from(format!("invalid status code: {status}")))?;
  let path = world.resolve_fixture_path(&fixture_path);
  let body = std::fs::read(&path).map_err(|e| StepError::from(format!("read fixture {}: {e}", path.display())))?;

  let content_type = match path.extension().and_then(|e| e.to_str()) {
    Some("json") => "application/json",
    Some("html") | Some("htm") => "text/html",
    Some("xml") => "application/xml",
    Some("txt") => "text/plain",
    Some("css") => "text/css",
    Some("js") => "application/javascript",
    _ => "application/octet-stream",
  }
  .to_string();

  let matcher =
    UrlMatcher::glob(&pattern).map_err(|e| StepError::from(format!("invalid url pattern \"{pattern}\": {e}")))?;
  world
    .page()
    .route(
      matcher,
      Arc::new(move |route| {
        route.fulfill(FulfillResponse {
          status,
          body: body.clone(),
          content_type: Some(content_type.clone()),
          ..FulfillResponse::default()
        });
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("mock with fixture \"{}\": {e}", fixture_path)))?;
}

#[given("I block requests to {string}")]
async fn block_requests(world: &mut BrowserWorld, pattern: String) {
  let matcher =
    UrlMatcher::glob(&pattern).map_err(|e| StepError::from(format!("invalid url pattern \"{pattern}\": {e}")))?;
  world
    .page()
    .route(
      matcher,
      Arc::new(|route| {
        route.abort("BlockedByClient");
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("block requests to \"{pattern}\": {e}")))?;
}

#[given("I intercept requests to {string}")]
async fn intercept_requests(world: &mut BrowserWorld, pattern: String) {
  let tracker = intercepted_requests(world);
  let matcher =
    UrlMatcher::glob(&pattern).map_err(|e| StepError::from(format!("invalid url pattern \"{pattern}\": {e}")))?;
  world
    .page()
    .route(
      matcher,
      Arc::new(move |route| {
        tracker.push(route.request().clone());
        route.continue_route(ContinueOverrides::default());
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("intercept requests to \"{pattern}\": {e}")))?;
}

#[when("I remove route for {string}")]
async fn remove_route(world: &mut BrowserWorld, pattern: String) {
  let matcher =
    UrlMatcher::glob(&pattern).map_err(|e| StepError::from(format!("invalid url pattern \"{pattern}\": {e}")))?;
  world
    .page()
    .unroute(&matcher)
    .await
    .map_err(|e| StepError::from(format!("remove route for \"{pattern}\": {e}")))?;
}

#[then("a request to {string} should have been made")]
async fn assert_request_made(world: &mut BrowserWorld, pattern: String) {
  let tracker = intercepted_requests(world);
  let count = tracker.count_matching(&pattern);
  if count == 0 {
    return Err(StepError::from(format!(
      "expected at least one request matching \"{pattern}\", but none were intercepted"
    )));
  }
}

#[then("{int} requests to {string} should have been made")]
async fn assert_request_count(world: &mut BrowserWorld, expected: i64, pattern: String) {
  let tracker = intercepted_requests(world);
  let actual = tracker.count_matching(&pattern);
  let expected_usize = usize::try_from(expected).map_err(|_| StepError::from(format!("invalid count: {expected}")))?;
  if actual != expected_usize {
    return Err(StepError {
      message: format!("expected {expected_usize} request(s) matching \"{pattern}\", but found {actual}"),
      diff: Some((expected_usize.to_string(), actual.to_string())),
      pending: false,
    });
  }
}

// ── Fetch + response assertion (cucumber-rest-bdd style) ───────────────

/// Last fetch response stored in world state for assertion chaining.
#[derive(Clone)]
struct LastFetchResponse {
  status: i32,
  body: String,
  headers: rustc_hash::FxHashMap<String, String>,
}

fn last_response(world: &BrowserWorld) -> Result<LastFetchResponse, StepError> {
  world
    .get_state::<LastFetchResponse>()
    .cloned()
    .ok_or_else(|| StepError::from("no fetch response stored -- use 'When I fetch \"...\"' first"))
}

#[when("I fetch {string}")]
async fn fetch_url(world: &mut BrowserWorld, url: String) {
  let js = format!(
    r#"(async () => {{
      const r = await fetch({url});
      const hdrs = {{}};
      r.headers.forEach((v, k) => {{ hdrs[k] = v; }});
      return JSON.stringify({{ status: r.status, body: await r.text(), headers: hdrs }});
    }})()"#,
    url = serde_json::to_string(&url).unwrap_or_else(|_| format!("\"{url}\""))
  );
  let result = world
    .page()
    .evaluate(&js, ferridriver::protocol::SerializedArgument::default(), None)
    .await
    .map_err(|e| StepError::from(format!("fetch \"{url}\": {e}")))?;
  let result = result.as_string_lossy();

  let parsed: serde_json::Value =
    serde_json::from_str(&result).map_err(|e| StepError::from(format!("parse fetch result: {e}")))?;

  let headers: rustc_hash::FxHashMap<String, String> = parsed
    .get("headers")
    .and_then(|h| h.as_object())
    .map(|obj| {
      obj
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
        .collect()
    })
    .unwrap_or_default();

  world.set_state(LastFetchResponse {
    status: parsed.get("status").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
    body: parsed.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    headers,
  });
}

#[then("the response status should be {int}")]
async fn response_status_should_be(world: &mut BrowserWorld, expected: i64) {
  let resp = last_response(world)?;
  let expected = expected as i32;
  if resp.status != expected {
    return Err(StepError {
      message: format!("expected response status {expected}, got {}", resp.status),
      diff: Some((expected.to_string(), resp.status.to_string())),
      pending: false,
    });
  }
}

#[then("the response body should contain {string}")]
async fn response_body_should_contain(world: &mut BrowserWorld, expected: String) {
  let resp = last_response(world)?;
  if !resp.body.contains(&expected) {
    return Err(StepError {
      message: format!("response body does not contain \"{expected}\""),
      diff: Some((expected, resp.body)),
      pending: false,
    });
  }
}

#[then("the response body should equal {string}")]
async fn response_body_should_equal(world: &mut BrowserWorld, expected: String) {
  let resp = last_response(world)?;
  if resp.body.trim() != expected.trim() {
    return Err(StepError {
      message: "response body does not match expected".to_string(),
      diff: Some((expected, resp.body)),
      pending: false,
    });
  }
}

#[then("the response header {string} should contain {string}")]
async fn response_header_should_contain(world: &mut BrowserWorld, header: String, expected: String) {
  let resp = last_response(world)?;
  let header_val = resp
    .headers
    .get(&header.to_lowercase())
    .map(String::as_str)
    .unwrap_or("");
  if !header_val.contains(&expected) {
    return Err(StepError {
      message: format!("response header \"{header}\" does not contain \"{expected}\" (got \"{header_val}\")"),
      diff: Some((expected, header_val.to_string())),
      pending: false,
    });
  }
}
