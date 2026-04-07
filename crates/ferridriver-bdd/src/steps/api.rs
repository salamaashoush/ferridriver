//! API request step definitions -- direct HTTP requests from BDD scenarios.
//!
//! Uses `APIRequestContext` from the core library for making HTTP requests
//! outside the browser context. Response stored in world for assertion chaining.
//!
//! ```gherkin
//! When I send a GET request to "/api/users"
//! When I send a POST request to "/api/users" with body:
//!   """
//!   {"name": "Alice"}
//!   """
//! Then the API response status should be 200
//! Then the API response body should contain "Alice"
//! Then the API response header "content-type" should contain "json"
//! ```

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver::api_request::{APIRequestContext, APIResponse, RequestContextOptions, RequestOptions};
use ferridriver_bdd_macros::{then, when};

/// Stored API response for assertion chaining.
struct LastAPIResponse(APIResponse);

fn get_or_create_ctx(world: &mut BrowserWorld) -> &APIRequestContext {
  if world.get_state::<APIRequestContext>().is_none() {
    world.set_state(APIRequestContext::new(RequestContextOptions::default()));
  }
  world.get_state::<APIRequestContext>().unwrap()
}

fn last_api_response(world: &BrowserWorld) -> Result<&APIResponse, StepError> {
  world
    .get_state::<LastAPIResponse>()
    .map(|r| &r.0)
    .ok_or_else(|| StepError::from("no API response stored -- use 'When I send a GET/POST/... request' first"))
}

// ── Request steps ──────────────────────────────────────────────────────

#[when("I send a GET request to {string}")]
async fn send_get(world: &mut BrowserWorld, url: String) {
  let ctx = get_or_create_ctx(world);
  let resp = ctx
    .get(&url, None)
    .await
    .map_err(|e| StepError::from(format!("GET {url}: {e}")))?;
  world.set_state(LastAPIResponse(resp));
}

#[when("I send a POST request to {string}")]
async fn send_post_no_body(world: &mut BrowserWorld, url: String) {
  let ctx = get_or_create_ctx(world);
  let resp = ctx
    .post(&url, None)
    .await
    .map_err(|e| StepError::from(format!("POST {url}: {e}")))?;
  world.set_state(LastAPIResponse(resp));
}

#[when("I send a POST request to {string} with body:")]
async fn send_post_with_body(world: &mut BrowserWorld, url: String, docstring: Option<&str>) {
  let body = docstring.unwrap_or("").to_string();
  let ctx = get_or_create_ctx(world);
  let opts = RequestOptions {
    json_data: serde_json::from_str(&body).ok(),
    data: if serde_json::from_str::<serde_json::Value>(&body).is_err() {
      Some(body.into_bytes())
    } else {
      None
    },
    ..Default::default()
  };
  let resp = ctx
    .post(&url, Some(opts))
    .await
    .map_err(|e| StepError::from(format!("POST {url}: {e}")))?;
  world.set_state(LastAPIResponse(resp));
}

#[when("I send a PUT request to {string} with body:")]
async fn send_put_with_body(world: &mut BrowserWorld, url: String, docstring: Option<&str>) {
  let body = docstring.unwrap_or("").to_string();
  let ctx = get_or_create_ctx(world);
  let opts = RequestOptions {
    json_data: serde_json::from_str(&body).ok(),
    data: if serde_json::from_str::<serde_json::Value>(&body).is_err() {
      Some(body.into_bytes())
    } else {
      None
    },
    ..Default::default()
  };
  let resp = ctx
    .put(&url, Some(opts))
    .await
    .map_err(|e| StepError::from(format!("PUT {url}: {e}")))?;
  world.set_state(LastAPIResponse(resp));
}

#[when("I send a DELETE request to {string}")]
async fn send_delete(world: &mut BrowserWorld, url: String) {
  let ctx = get_or_create_ctx(world);
  let resp = ctx
    .delete(&url, None)
    .await
    .map_err(|e| StepError::from(format!("DELETE {url}: {e}")))?;
  world.set_state(LastAPIResponse(resp));
}

#[when("I send a PATCH request to {string} with body:")]
async fn send_patch_with_body(world: &mut BrowserWorld, url: String, docstring: Option<&str>) {
  let body = docstring.unwrap_or("").to_string();
  let ctx = get_or_create_ctx(world);
  let opts = RequestOptions {
    json_data: serde_json::from_str(&body).ok(),
    data: if serde_json::from_str::<serde_json::Value>(&body).is_err() {
      Some(body.into_bytes())
    } else {
      None
    },
    ..Default::default()
  };
  let resp = ctx
    .patch(&url, Some(opts))
    .await
    .map_err(|e| StepError::from(format!("PATCH {url}: {e}")))?;
  world.set_state(LastAPIResponse(resp));
}

// ── Response assertion steps ───────────────────────────────────────────

#[then("the API response status should be {int}")]
async fn api_response_status(world: &mut BrowserWorld, expected: i64) {
  let resp = last_api_response(world)?;
  let actual = resp.status() as i64;
  if actual != expected {
    return Err(StepError {
      message: format!("expected API response status {expected}, got {actual}"),
      diff: Some((expected.to_string(), actual.to_string())),
      pending: false,
    });
  }
}

#[then("the API response should be successful")]
async fn api_response_ok(world: &mut BrowserWorld) {
  let resp = last_api_response(world)?;
  if !resp.ok() {
    return Err(StepError::from(format!(
      "expected successful API response (2xx), got {}",
      resp.status()
    )));
  }
}

#[then("the API response body should contain {string}")]
async fn api_response_body_contains(world: &mut BrowserWorld, expected: String) {
  let resp = last_api_response(world)?;
  let body = resp.text().map_err(|e| StepError::from(e.to_string()))?;
  if !body.contains(&expected) {
    return Err(StepError {
      message: format!("API response body does not contain \"{expected}\""),
      diff: Some((expected, body)),
      pending: false,
    });
  }
}

#[then("the API response body should equal {string}")]
async fn api_response_body_equals(world: &mut BrowserWorld, expected: String) {
  let resp = last_api_response(world)?;
  let body = resp.text().map_err(|e| StepError::from(e.to_string()))?;
  if body.trim() != expected.trim() {
    return Err(StepError {
      message: "API response body does not match expected".to_string(),
      diff: Some((expected, body)),
      pending: false,
    });
  }
}

#[then("the API response header {string} should contain {string}")]
async fn api_response_header_contains(world: &mut BrowserWorld, header: String, expected: String) {
  let resp = last_api_response(world)?;
  let header_val = resp.header(&header).unwrap_or("");
  if !header_val.contains(&expected) {
    return Err(StepError {
      message: format!(
        "API response header \"{header}\" does not contain \"{expected}\" (got \"{header_val}\")"
      ),
      diff: Some((expected, header_val.to_string())),
      pending: false,
    });
  }
}
