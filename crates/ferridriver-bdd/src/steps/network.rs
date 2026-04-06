//! Network interception/mocking step definitions.
//!
//! Uses the `page.route()` / `page.unroute()` API to intercept, mock,
//! and block network requests during BDD scenarios.

use std::sync::{Arc, Mutex};

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver::route::{FulfillResponse, InterceptedRequest, RouteAction};
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
  let status = i32::try_from(status)
    .map_err(|_| StepError::from(format!("invalid status code: {status}")))?;
  let body_bytes = body.into_bytes();
  world
    .page()
    .route(
      &pattern,
      Arc::new(move |_req: &InterceptedRequest| {
        RouteAction::Fulfill(FulfillResponse {
          status,
          body: body_bytes.clone(),
          ..FulfillResponse::default()
        })
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("mock requests to \"{pattern}\": {e}")))?;
}

#[given("I mock requests to {string} with JSON {string}")]
async fn mock_with_json(world: &mut BrowserWorld, pattern: String, json_body: String) {
  let body_bytes = json_body.into_bytes();
  world
    .page()
    .route(
      &pattern,
      Arc::new(move |_req: &InterceptedRequest| {
        RouteAction::Fulfill(FulfillResponse {
          status: 200,
          body: body_bytes.clone(),
          content_type: Some("application/json".to_string()),
          ..FulfillResponse::default()
        })
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("mock JSON requests to \"{pattern}\": {e}")))?;
}

#[given("I block requests to {string}")]
async fn block_requests(world: &mut BrowserWorld, pattern: String) {
  world
    .page()
    .route(
      &pattern,
      Arc::new(|_req: &InterceptedRequest| RouteAction::Abort("BlockedByClient".to_string())),
    )
    .await
    .map_err(|e| StepError::from(format!("block requests to \"{pattern}\": {e}")))?;
}

#[given("I intercept requests to {string}")]
async fn intercept_requests(world: &mut BrowserWorld, pattern: String) {
  let tracker = intercepted_requests(world);
  world
    .page()
    .route(
      &pattern,
      Arc::new(move |req: &InterceptedRequest| {
        tracker.push(req.clone());
        RouteAction::Continue(Default::default())
      }),
    )
    .await
    .map_err(|e| StepError::from(format!("intercept requests to \"{pattern}\": {e}")))?;
}

#[when("I remove route for {string}")]
async fn remove_route(world: &mut BrowserWorld, pattern: String) {
  world
    .page()
    .unroute(&pattern)
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
  let expected_usize = usize::try_from(expected)
    .map_err(|_| StepError::from(format!("invalid count: {expected}")))?;
  if actual != expected_usize {
    return Err(StepError {
      message: format!(
        "expected {expected_usize} request(s) matching \"{pattern}\", but found {actual}"
      ),
      diff: Some((expected_usize.to_string(), actual.to_string())),
    });
  }
}
