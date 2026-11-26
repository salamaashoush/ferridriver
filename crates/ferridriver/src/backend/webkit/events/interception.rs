//! `WebKit` `Network.requestIntercepted` route matching and execution.

use std::sync::Arc;

use serde_json::{Value, json};

use super::dispatch::Routes;

/// Translate `Network.requestIntercepted` into a matched
/// [`crate::route::Route`] dispatch. When no route matches, continue
/// unmodified.
pub(super) fn handle_request_intercepted(
  params: &Value,
  target: &crate::backend::webkit::connection::Session,
  routes: &Routes,
) {
  let request_id = params
    .get("requestId")
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_string();
  if request_id.is_empty() {
    return;
  }
  let request_payload = params.get("request").cloned().unwrap_or(Value::Null);
  let target = target.clone();
  let routes = Arc::clone(routes);
  tokio::spawn(async move { dispatch_intercepted(target, routes, request_id, request_payload).await });
}

async fn dispatch_intercepted(
  target: crate::backend::webkit::connection::Session,
  routes: Routes,
  request_id: String,
  request_payload: Value,
) {
  let intercepted = build_intercepted(&request_id, &request_payload);
  let has_match = {
    let guard = routes.read().await;
    crate::route::any_matching_route(&guard, &intercepted.url)
  };
  if !has_match {
    let _ = target
      .send(
        "Network.interceptContinue",
        json!({ "requestId": request_id, "stage": "request" }),
      )
      .await;
    return;
  }
  let action = crate::route::run_route_chain(&routes, intercepted).await;
  match action {
    crate::route::RouteAction::Continue(overrides) | crate::route::RouteAction::Fallback(overrides) => {
      intercept_continue(&target, &request_id, overrides).await;
    },
    // Chain-internal marker; never emitted as a terminal action. Fail
    // open like an unhandled route if it ever leaks.
    crate::route::RouteAction::PredicateRejected => {
      intercept_continue(&target, &request_id, crate::route::ContinueOverrides::default()).await;
    },
    crate::route::RouteAction::Fulfill(response) => intercept_fulfill(&target, &request_id, &response).await,
    crate::route::RouteAction::Abort(reason) => {
      // Mirror Playwright's abort-reason → WebKit ResourceErrorType map
      // (`wkInterceptableRequest.ts:28-43`). `Cancellation` reads as a
      // benign user stop — an aborted NAVIGATION then resolves `goto`
      // instead of failing it — so only the reasons Playwright maps to
      // Cancellation get it; everything else (incl. the default
      // `failed`) is `General`.
      let error_type = match reason.to_lowercase().as_str() {
        "aborted" | "blockedbyclient" => "Cancellation",
        "accessdenied" => "AccessControl",
        "timedout" => "Timeout",
        _ => "General",
      };
      let _ = target
        .send(
          "Network.interceptRequestWithError",
          json!({ "requestId": request_id, "errorType": error_type }),
        )
        .await;
    },
  }
}

fn build_intercepted(request_id: &str, request_payload: &Value) -> crate::route::InterceptedRequest {
  let url = request_payload
    .get("url")
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_string();
  let method = request_payload
    .get("method")
    .and_then(Value::as_str)
    .unwrap_or("GET")
    .to_string();
  // Base64 on the wire, as on `Network.requestWillBeSent`
  // (`wkInterceptableRequest.ts`).
  let post_data = request_payload
    .get("postData")
    .and_then(Value::as_str)
    .and_then(|encoded| {
      use base64::Engine as _;
      let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
      Some(String::from_utf8_lossy(&bytes).into_owned())
    });
  let mut headers = rustc_hash::FxHashMap::default();
  if let Some(map) = request_payload.get("headers").and_then(Value::as_object) {
    for (k, v) in map {
      if let Some(s) = v.as_str() {
        headers.insert(k.clone(), s.to_string());
      }
    }
  }
  crate::route::InterceptedRequest {
    request_id: request_id.to_string(),
    url,
    method,
    headers,
    post_data,
    resource_type: "Other".to_string(),
  }
}

async fn intercept_continue(
  target: &crate::backend::webkit::connection::Session,
  request_id: &str,
  overrides: crate::route::ContinueOverrides,
) {
  use base64::Engine as _;
  if overrides.url.is_none()
    && overrides.method.is_none()
    && overrides.headers.is_none()
    && overrides.post_data.is_none()
  {
    let _ = target
      .send(
        "Network.interceptContinue",
        json!({ "requestId": request_id, "stage": "request" }),
      )
      .await;
    return;
  }
  let mut params = json!({ "requestId": request_id });
  if let Some(u) = overrides.url {
    params["url"] = json!(u);
  }
  if let Some(m) = overrides.method {
    params["method"] = json!(m);
  }
  if let Some(h) = overrides.headers {
    let mut headers_map = serde_json::Map::new();
    for (k, v) in h {
      headers_map.insert(k, Value::String(v));
    }
    params["headers"] = Value::Object(headers_map);
  }
  if let Some(body) = overrides.post_data {
    params["postData"] = json!(base64::engine::general_purpose::STANDARD.encode(&body));
  }
  let _ = target.send("Network.interceptWithRequest", params).await;
}

pub(super) fn fulfilled_mime_type(response: &crate::route::FulfillResponse) -> String {
  let named = response.content_type.as_deref().or_else(|| {
    response
      .headers
      .iter()
      .rev()
      .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
      .map(|(_, value)| value.as_str())
  });
  let essence = named.map(|named| named.split_once(';').map_or(named, |(essence, _)| essence).trim());
  match essence {
    Some(essence) if !essence.is_empty() => essence.to_string(),
    _ => crate::route::sniff_content_type(&response.body).to_string(),
  }
}

async fn intercept_fulfill(
  target: &crate::backend::webkit::connection::Session,
  request_id: &str,
  response: &crate::route::FulfillResponse,
) {
  use base64::Engine as _;
  let mut headers_map = serde_json::Map::new();
  // WebKit's interception never sniffs — the `mimeType` param decides how
  // the document is parsed. When the handler names no content type, run
  // the WHATWG sniffing step ourselves so fulfill renders the same as on
  // CDP/BiDi (where the browser sniffs the header-less response).
  let mime_type = fulfilled_mime_type(response);
  for (k, v) in response.effective_headers() {
    headers_map.insert(k, Value::String(v));
  }
  let content_b64 = base64::engine::general_purpose::STANDARD.encode(&response.body);
  let status_text = crate::route::status_text(response.status);
  let _ = target
    .send(
      "Network.interceptRequestWithResponse",
      json!({
        "requestId": request_id,
        "content": content_b64,
        "base64Encoded": true,
        "mimeType": mime_type,
        "status": response.status,
        "statusText": status_text,
        "headers": Value::Object(headers_map),
      }),
    )
    .await;
}

/// Translate a `Page.fileChooserOpened` event into a live
/// [`crate::file_chooser::FileChooser`] and dispatch through the
/// page's manager.
pub(super) fn dispatch_file_chooser(
  params: &Value,
  target: &crate::backend::webkit::connection::Session,
  page_backref: &crate::backend::PageBackref,
  manager: &crate::file_chooser::FileChooserManager,
) {
  let Some(object_id) = params
    .get("element")
    .and_then(|e| e.get("objectId"))
    .and_then(Value::as_str)
  else {
    return;
  };
  let object_id = object_id.to_string();
  let Some(page) = page_backref.upgrade() else {
    return;
  };
  let target = target.clone();
  let manager = manager.clone();
  tokio::spawn(async move {
    // Inspect `element.multiple` lazily so the per-event dispatch
    // surfaces an accurate `isMultiple()`. PW `WebKit`'s
    // `fileChooserOpened` payload doesn't carry the field.
    let is_multiple_resp = target
      .send(
        "Runtime.callFunctionOn",
        json!({
          "objectId": object_id,
          "functionDeclaration": "function(){return !!this.multiple}",
          "returnByValue": true,
          "awaitPromise": false,
        }),
      )
      .await;
    let is_multiple = is_multiple_resp
      .ok()
      .as_ref()
      .and_then(|r| r.get("result"))
      .and_then(|r| r.get("value"))
      .and_then(Value::as_bool)
      .unwrap_or(false);
    let element = crate::backend::webkit::element::WebKitElement::new(target, object_id);
    let any_element = crate::backend::AnyElement::WebKit(element);
    crate::backend::shared_page::dispatch_file_chooser_event(page, &manager, any_element, is_multiple).await;
  });
}
