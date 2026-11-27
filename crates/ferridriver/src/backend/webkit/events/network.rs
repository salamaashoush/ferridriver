//! `WebKit` network and `WebSocket` event handlers.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;

use super::Requests;
use crate::network::{
  BodyFn, Headers, RemoteAddr, Request as NetworkRequest, RequestInit, Response, ResponseInit, SecurityDetails,
  WebSocket,
};

/// Push a fresh [`NetworkRequest`] into the per-page table and the
/// context log. When `redirectResponse` is present, link the chain
/// (the prior request's `redirected_to` slot points at the new one).
pub(super) async fn handle_request_will_be_sent(
  params: &Value,
  requests: &Requests,
  nav_slot: &crate::network::NavRequestSlot,
  network_log: &Arc<RwLock<Vec<NetworkRequest>>>,
  emitter: &crate::events::EventEmitter,
  page_proxy_id: &str,
) {
  let Some(request_payload) = params.get("request") else {
    return;
  };
  let request_id = params
    .get("requestId")
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_string();
  if request_id.is_empty() {
    return;
  }

  // Redirect chain: when this request follows a redirect, the prior
  // request is in our table at the same `requestId` (PW WebKit reuses
  // the id across the redirect). Finalize the prior with the redirect
  // response and link the chain.
  let redirected_from: Option<NetworkRequest> = if let Some(redir) = params.get("redirectResponse") {
    let prev = requests
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .remove(&request_id);
    if let Some(ref prev) = prev {
      let response = build_response(prev.clone(), redir, None);
      prev.set_response(&response).await;
      emitter.emit(crate::events::PageEvent::Response(response));
      emitter.emit(crate::events::PageEvent::RequestFinished(prev.clone()));
    }
    prev
  } else {
    None
  };

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
  let headers = request_payload
    .get("headers")
    .and_then(Value::as_object)
    .map_or_else(Headers::default, |map| {
      let mut headers = Headers::with_capacity_and_hasher(map.len(), rustc_hash::FxBuildHasher);
      for (k, v) in map {
        if let Some(s) = v.as_str() {
          headers.insert(k.clone(), s.to_string());
        }
      }
      headers
    });
  let resource_type = params
    .get("type")
    .and_then(Value::as_str)
    .unwrap_or("Other")
    .to_string();
  let is_navigation_request = resource_type.eq_ignore_ascii_case("Document");
  let req = NetworkRequest::new(RequestInit {
    id: request_id.clone(),
    url,
    method,
    resource_type,
    is_navigation_request,
    post_data: request_payload.get("postData").and_then(Value::as_str).and_then(|s| {
      use base64::Engine as _;
      base64::engine::general_purpose::STANDARD.decode(s).ok()
    }),
    headers,
    frame_id: params.get("frameId").and_then(Value::as_str).map(String::from),
    page_guid: Some(page_proxy_id.to_string()),
    redirected_from,
    // `walltime` is epoch seconds (wkInterceptableRequest.ts: `* 1000`).
    timing: params
      .get("walltime")
      .and_then(Value::as_f64)
      .filter(|wall| *wall > 0.0)
      .map(|wall| crate::network::RequestTiming {
        start_time: wall * 1000.0,
        ..crate::network::RequestTiming::empty()
      }),
    raw_headers_fn: None,
  });
  requests
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .insert(request_id, req.clone());
  crate::state::push_capped(
    &mut *network_log.write().await,
    req.clone(),
    crate::state::NETWORK_LOG_CAP,
  );
  if is_navigation_request {
    match params.get("loaderId").and_then(Value::as_str) {
      Some(loader_id) => nav_slot.set_for_loader(req.clone(), loader_id.to_string()),
      None => nav_slot.set(req.clone()),
    }
  }
  emitter.emit(crate::events::PageEvent::Request(req));
}

/// What Playwright's `WebKit` build joins repeated `Set-Cookie` values
/// with. A comma is ambiguous inside a cookie's `Expires` date, so off
/// macOS the build substitutes a marker no header can contain
/// (`wkInterceptableRequest.ts::wkSetCookieSeparator`).
const SET_COOKIE_SEPARATOR: &str = if cfg!(target_os = "macos") {
  ","
} else {
  "playwright-set-cookie-separator"
};

/// Build a [`Response`] for `request` from a PW `WebKit`
/// `response` JSON payload. When `target` is `Some`, attach a body
/// fetcher closure that issues `Network.getResponseBody({requestId})`
/// on demand.
fn build_response(request: NetworkRequest, response: &Value, body_fn: Option<BodyFn>) -> Response {
  let url = response.get("url").and_then(Value::as_str).unwrap_or("").to_string();
  let status = response.get("status").and_then(Value::as_i64).unwrap_or(0);
  let status_text = response
    .get("statusText")
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_string();
  let headers = response
    .get("headers")
    .and_then(Value::as_object)
    .map_or_else(Headers::default, |map| {
      let mut headers = Headers::with_capacity_and_hasher(map.len(), rustc_hash::FxBuildHasher);
      for (k, v) in map {
        if let Some(s) = v.as_str() {
          headers.insert(k.clone(), s.to_string());
        }
      }
      headers
    });
  // The protocol reports response headers as a map, so a header the
  // server sent twice arrives joined and would otherwise be one entry.
  // Playwright splits the same value the same way
  // (`wkInterceptableRequest.ts`: `headersObjectToArray(headers, ',',
  // wkSetCookieSeparator)`), and its WebKit build joins `Set-Cookie`
  // with a marker off macOS precisely so that one can be split back
  // without guessing.
  let raw_headers = crate::network::headers_map_to_split_array(&headers, ",", SET_COOKIE_SEPARATOR);
  let remote_addr = response
    .get("remoteIPAddress")
    .and_then(Value::as_str)
    .map(|ip| RemoteAddr {
      ip_address: ip.to_string(),
      port: u16::try_from(response.get("remotePort").and_then(Value::as_u64).unwrap_or(0)).unwrap_or(0),
    });
  // `wkPage.ts` reads the certificate the same way; WebKit reports no
  // issuer.
  let security_details: Option<SecurityDetails> = response.pointer("/security/certificate").map(|c| SecurityDetails {
    protocol: None,
    subject_name: c.get("subject").and_then(Value::as_str).map(String::from),
    issuer: None,
    valid_from: c.get("validFrom").and_then(Value::as_f64),
    valid_to: c.get("validUntil").and_then(Value::as_f64),
  });
  Response::new(ResponseInit {
    request,
    url,
    status,
    status_text,
    from_service_worker: false,
    http_version: response
      .get("protocol")
      .and_then(Value::as_str)
      .map(std::string::ToString::to_string),
    headers,
    remote_addr,
    security_details,
    body_fn,
    raw_headers_fn: Some(crate::network::known_raw_headers(raw_headers)),
  })
}

pub(super) async fn handle_response_received(
  params: &Value,
  requests: &Requests,
  target: &crate::backend::webkit::connection::Session,
  emitter: &crate::events::EventEmitter,
) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let Some(req) = requests
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .get(request_id)
    .cloned()
  else {
    return;
  };
  let Some(response_payload) = params.get("response") else {
    return;
  };
  let body_fn = build_body_fn(target.clone(), request_id.to_string());
  let response = build_response(req.clone(), response_payload, Some(body_fn));
  req.set_response(&response).await;
  emitter.emit(crate::events::PageEvent::Response(response));
}

/// Build a body-fetcher closure that runs `Network.getResponseBody`
/// against the given target session.
fn build_body_fn(target: crate::backend::webkit::connection::Session, request_id: String) -> BodyFn {
  use base64::Engine as _;
  Arc::new(move || {
    let target = target.clone();
    let request_id = request_id.clone();
    Box::pin(async move {
      let resp = target
        .send_typed(&ferridriver_webkit_proto::network::GetResponseBody {
          request_id: &request_id,
        })
        .await
        .map_err(|e| crate::error::FerriError::backend(format!("getResponseBody: {e}")))?;
      if resp.base64_encoded {
        base64::engine::general_purpose::STANDARD
          .decode(&resp.body)
          .map_err(|e| crate::error::FerriError::backend(format!("getResponseBody base64: {e}")))
      } else {
        Ok(resp.body.into_bytes())
      }
    })
  })
}

/// `Network.loadingFinished`: the response is complete, and its metrics
/// carry the server's address (`wkPage.ts::_onLoadingFinished`).
pub(super) async fn handle_loading_finished(
  params: &Value,
  requests: &Requests,
  emitter: &crate::events::EventEmitter,
) {
  let request_id = params.get("requestId").and_then(Value::as_str).unwrap_or("");
  let req = requests
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .remove(request_id);
  if let Some(req) = req {
    if let Some(metrics) = params.get("metrics") {
      let request_headers = metrics
        .get("requestHeaderBytesSent")
        .and_then(Value::as_u64)
        .unwrap_or(0);
      let request_body = metrics.get("requestBodyBytesSent").and_then(Value::as_u64).unwrap_or(0);
      let response_headers = metrics
        .get("responseHeaderBytesReceived")
        .and_then(Value::as_u64)
        .unwrap_or(0);
      let response_body = metrics
        .get("responseBodyBytesReceived")
        .and_then(Value::as_u64)
        .unwrap_or(0);
      if request_headers > 0 || request_body > 0 || response_headers > 0 || response_body > 0 {
        req.update_sizes(crate::network::RequestSizes {
          request_body,
          request_headers,
          response_body,
          response_headers,
        });
      }
    }
    if let Some(response) = req.existing_response().await {
      if let Some(addr) = params
        .pointer("/metrics/remoteAddress")
        .and_then(Value::as_str)
        .and_then(parse_remote_address)
      {
        response.set_server_addr(addr).await;
      }
      if let Some(protocol) = params.pointer("/metrics/protocol").and_then(Value::as_str) {
        response.set_http_version(protocol.to_string()).await;
      }
      if let Some(tls_protocol) = params
        .pointer("/metrics/securityConnection/protocol")
        .and_then(Value::as_str)
      {
        response.set_security_protocol(tls_protocol.to_string()).await;
      }
      response.finish_success().await;
    }
    emitter.emit(crate::events::PageEvent::RequestFinished(req));
  }
}

pub(super) async fn handle_loading_failed(params: &Value, requests: &Requests, emitter: &crate::events::EventEmitter) {
  let request_id = params.get("requestId").and_then(Value::as_str).unwrap_or("");
  let req = requests
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .remove(request_id);
  if let Some(req) = req {
    let error_text = params
      .get("errorText")
      .and_then(Value::as_str)
      .unwrap_or("")
      .to_string();
    if let Some(response) = req.existing_response().await {
      response
        .finish_failure(crate::error::FerriError::backend(error_text.clone()))
        .await;
    }
    req.set_failure(error_text);
    emitter.emit(crate::events::PageEvent::RequestFailed(req));
  }
}

/// `host:port` as `WebKit` reports it, an IPv6 host bare and bracketed in
/// the result (`wkPage.ts::parseRemoteAddress`).
pub(super) fn parse_remote_address(value: &str) -> Option<RemoteAddr> {
  let (host, port) = value.rsplit_once(':')?;
  let port = port.parse().ok()?;
  let ip_address = if host.contains(':') && !host.starts_with('[') {
    format!("[{host}]")
  } else {
    host.to_string()
  };
  Some(RemoteAddr { ip_address, port })
}

/// Convert one PW `WebKit` `Runtime.RemoteObject` JSON payload into a
/// [`crate::js_handle::JSHandleBacking`]. Object-id-bearing objects
/// become remote-backed handles; primitives ride back as
/// [`crate::js_handle::JSHandleBacking::Value`].
pub(super) fn pw_remote_object_to_backing(arg: &Value) -> crate::js_handle::JSHandleBacking {
  use crate::js_handle::{HandleRemote, JSHandleBacking};
  use crate::protocol::{SerializationContext, SerializedValue, SpecialValue};

  if let Some(obj_id) = arg.get("objectId").and_then(Value::as_str) {
    return JSHandleBacking::Remote(HandleRemote::WebKit(Arc::from(obj_id)));
  }
  let serialized = match arg.get("value") {
    None | Some(Value::Null) => {
      if arg.get("type").and_then(Value::as_str) == Some("undefined") {
        SerializedValue::Special(SpecialValue::Undefined)
      } else {
        SerializedValue::Special(SpecialValue::Null)
      }
    },
    Some(value) => SerializedValue::from_json(value, &mut SerializationContext::default()),
  };
  JSHandleBacking::Value(serialized)
}

pub(super) type WebSockets = Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, WebSocket>>>;

/// `Network.webSocketCreated` → register live [`WebSocket`] keyed by
/// `requestId` and emit [`PageEvent::WebSocket`] so user code attached
/// via `page.waitForEvent('websocket')` can grab it.
pub(super) fn handle_websocket_created(params: &Value, websockets: &WebSockets, emitter: &crate::events::EventEmitter) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let url = params.get("url").and_then(Value::as_str).unwrap_or("").to_string();
  let ws = WebSocket::new(url);
  websockets
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .insert(request_id.to_string(), ws.clone());
  emitter.emit(crate::events::PageEvent::WebSocket(ws));
}

/// `Network.webSocketFrame{Sent,Received}` → emit one frame on the live
/// [`WebSocket`]. Opcode 2 → binary (base64-decoded), otherwise text.
pub(super) fn handle_websocket_frame(params: &Value, websockets: &WebSockets, sent: bool) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let Some(payload) = crate::network::websocket_frame_payload(params) else {
    return;
  };
  let map = websockets.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  if let Some(ws) = map.get(request_id) {
    if sent {
      ws.emit_frame_sent(payload);
    } else {
      ws.emit_frame_received(payload);
    }
  }
}

pub(super) fn handle_websocket_error(params: &Value, websockets: &WebSockets) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let message = params
    .get("errorMessage")
    .and_then(Value::as_str)
    .unwrap_or("WebSocket error")
    .to_string();
  if let Some(ws) = websockets
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .get(request_id)
  {
    ws.emit_error(message);
  }
}

pub(super) fn handle_websocket_closed(params: &Value, websockets: &WebSockets) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let closed = websockets
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .remove(request_id);
  if let Some(ws) = closed {
    ws.emit_close();
  }
}
