//! CDP per-page network request/response tracking, `WebMCP` invocation guard, and cookie conversion.

use super::*;

// ── NetworkTracker ─────────────────────────────────────────────────────────
//
// Per-page bookkeeping for live `Request` / `Response` / `WebSocket`
// objects. CDP delivers network events out of order — `responseReceived`
// can fire before `requestWillBeSentExtraInfo`; `loadingFinished` arrives
// independently. Tracker indexes by request id and threads the same
// `Request` Arc through every event so listeners see live state.

pub(super) struct NetworkTracker {
  transport: Arc<CdpTransport>,
  session_id: Option<Arc<str>>,
  /// Owning page's target id — stamped on every captured `Request` as
  /// its `page_guid` for context-level page attribution (HAR pages,
  /// page-scoped HAR update recording).
  target_id: Arc<str>,
  requests: std::sync::Mutex<FxHashMap<String, network::Request>>,
  responses: std::sync::Mutex<FxHashMap<String, Response>>,
  websockets: std::sync::Mutex<FxHashMap<String, WebSocket>>,
  // Buffered extra-info events that may arrive before the main events.
  pending_request_extra: std::sync::Mutex<FxHashMap<String, Vec<HeaderEntry>>>,
  pending_response_extra: std::sync::Mutex<FxHashMap<String, Vec<HeaderEntry>>>,
  nav_request_slot: crate::network::NavRequestSlot,
}

fn lock_map<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Build an optional raw-headers fallback for provisional CDP headers without
/// capturing the [`NetworkTracker`] (which would form an `Arc` cycle through
/// `requests`/`responses`). When no provisional value carries CDP's `\n`
/// multi-value join, `None` lets [`Request`]/[`Response`] fall back directly
/// to `provisional_headers` with zero closure allocation.
fn provisional_raw_headers_fn(headers: &Headers) -> Option<RawHeadersFn> {
  headers
    .values()
    .any(|v| v.contains('\n'))
    .then(|| crate::network::known_raw_headers(crate::network::headers_map_to_split_array(headers, "\n", "\n")))
}

impl NetworkTracker {
  pub(super) fn new(
    transport: Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    target_id: Arc<str>,
    nav_request_slot: crate::network::NavRequestSlot,
  ) -> Self {
    Self {
      transport,
      session_id,
      target_id,
      requests: std::sync::Mutex::new(FxHashMap::default()),
      responses: std::sync::Mutex::new(FxHashMap::default()),
      websockets: std::sync::Mutex::new(FxHashMap::default()),
      pending_request_extra: std::sync::Mutex::new(FxHashMap::default()),
      pending_response_extra: std::sync::Mutex::new(FxHashMap::default()),
      nav_request_slot,
    }
  }

  pub(super) async fn on_request_will_be_sent(
    self: &Arc<Self>,
    params: &serde_json::Value,
    network_log: &Arc<RwLock<Vec<NetworkRequest>>>,
    emitter: &crate::events::EventEmitter,
  ) {
    let request_id = params
      .get("requestId")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    if request_id.is_empty() {
      return;
    }
    let req = params.get("request");
    let url = req
      .and_then(|r| r.get("url"))
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    let method = req
      .and_then(|r| r.get("method"))
      .and_then(|v| v.as_str())
      .unwrap_or("GET")
      .to_string();
    let resource_type = params.get("type").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let headers = req
      .and_then(|r| r.get("headers"))
      .and_then(|h| h.as_object())
      .map(|obj| {
        obj
          .iter()
          .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
          .collect::<Headers>()
      })
      .unwrap_or_default();
    let post_data = req
      .and_then(|r| r.get("postData"))
      .and_then(|v| v.as_str())
      .map(|s| s.as_bytes().to_vec());
    let frame_id = params
      .get("frameId")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let is_navigation_request = params
      .get("loaderId")
      .and_then(|v| v.as_str())
      .is_some_and(|loader| params.get("requestId").and_then(|v| v.as_str()) == Some(loader));

    // Redirect detection: when redirectResponse is set we synthesise a
    // Response on the prior request, then create a NEW request linked
    // to it via redirected_from. Same CDP requestId is reused for the
    // chain, so we re-key the prior in the tracker map.
    let redirected_from = if let Some(redirect_resp) = params.get("redirectResponse") {
      let prev = lock_map(&self.requests).remove(&request_id);
      if let Some(prev) = prev {
        let synthesised = self.build_response_from_value(prev.clone(), redirect_resp, &request_id);
        prev.set_response(&synthesised).await;
        synthesised.finish_success().await;
        emitter.emit(crate::events::PageEvent::Response(synthesised));
        emitter.emit(crate::events::PageEvent::RequestFinished(prev.clone()));
        Some(prev)
      } else {
        None
      }
    } else {
      None
    };

    let raw_headers_fn = provisional_raw_headers_fn(&headers);

    // `wallTime` is epoch seconds — the one wall-clock sample CDP
    // provides for a request (`ResourceTiming.requestTime` is monotonic).
    let timing = params
      .get("wallTime")
      .and_then(serde_json::Value::as_f64)
      .filter(|wall| *wall > 0.0)
      .map(|wall| network::RequestTiming {
        start_time: wall * 1000.0,
        ..network::RequestTiming::empty()
      });

    let new_request = network::Request::new(RequestInit {
      id: request_id.clone(),
      url,
      method,
      resource_type,
      is_navigation_request,
      post_data,
      headers,
      frame_id,
      page_guid: Some(self.target_id.to_string()),
      redirected_from,
      timing,
      raw_headers_fn,
    });

    let extras = lock_map(&self.pending_request_extra).remove(&request_id);
    if let Some(extras) = extras {
      new_request.set_raw_headers(extras).await;
    }

    lock_map(&self.requests).insert(request_id, new_request.clone());

    // Main-document navigations: update the per-page slot so
    // `AnyPage::goto` / `reload` / history traversals can resolve the
    // final `Response` after the lifecycle waiter fires. CDP flags a
    // request as navigation by setting `loaderId == requestId`; the
    // slot therefore tracks each redirect hop (same requestId, reused
    // across the chain) and naturally ends up pointing at the final
    // request in the chain when redirects settle. A child frame's
    // document is a navigation request too, and loads after the page's
    // own; a page target's main frame has the target's id.
    if new_request.is_navigation_request() && new_request.frame_id() == Some(&*self.target_id) {
      self.nav_request_slot.set(new_request.clone());
    }

    crate::state::push_capped(
      &mut *network_log.write().await,
      new_request.clone(),
      crate::state::NETWORK_LOG_CAP,
    );
    emitter.emit(crate::events::PageEvent::Request(new_request));
  }

  pub(super) async fn on_request_extra_info(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let raw = parse_raw_headers(params.get("headers"));
    let req = lock_map(&self.requests).get(request_id).cloned();
    if let Some(req) = req {
      req.set_raw_headers(raw).await;
    } else {
      lock_map(&self.pending_request_extra).insert(request_id.to_string(), raw);
    }
  }

  pub(super) async fn on_response_received(
    self: &Arc<Self>,
    params: &serde_json::Value,
    emitter: &crate::events::EventEmitter,
  ) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let Some(req) = lock_map(&self.requests).get(request_id).cloned() else {
      return;
    };
    let Some(resp_value) = params.get("response") else {
      return;
    };
    let response = self.build_response_from_value(req.clone(), resp_value, request_id);
    let extras = lock_map(&self.pending_response_extra).remove(request_id);
    if let Some(extras) = extras {
      response.set_raw_headers(extras).await;
    }
    lock_map(&self.responses).insert(request_id.to_string(), response.clone());
    req.set_response(&response).await;
    emitter.emit(crate::events::PageEvent::Response(response));
  }

  pub(super) async fn on_response_extra_info(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let raw = parse_raw_headers(params.get("headers"));
    let resp = lock_map(&self.responses).get(request_id).cloned();
    if let Some(resp) = resp {
      resp.set_raw_headers(raw).await;
    } else {
      lock_map(&self.pending_response_extra).insert(request_id.to_string(), raw);
    }
  }

  pub(super) async fn on_loading_finished(
    self: &Arc<Self>,
    params: &serde_json::Value,
    emitter: &crate::events::EventEmitter,
  ) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let Some(req) = lock_map(&self.requests).get(request_id).cloned() else {
      return;
    };
    let total_encoded = params
      .get("encodedDataLength")
      .and_then(serde_json::Value::as_f64)
      .unwrap_or(0.0);
    if total_encoded > 0.0 {
      // Best-effort allocation of `encodedDataLength` to body vs
      // headers. CDP doesn't split it — Playwright treats the full
      // value as response body size for `responseBodySize` and
      // queries `getResponseBody` for the actual body byte length on
      // demand. We mirror that: store raw transfer length here.
      req.update_sizes(RequestSizes {
        response_body: f64_to_u64_saturating(total_encoded),
        ..RequestSizes::default()
      });
    }
    let resp = lock_map(&self.responses).get(request_id).cloned();
    if let Some(resp) = resp {
      resp.finish_success().await;
    }
    emitter.emit(crate::events::PageEvent::RequestFinished(req));
  }

  pub(super) async fn on_loading_failed(
    self: &Arc<Self>,
    params: &serde_json::Value,
    emitter: &crate::events::EventEmitter,
  ) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let Some(req) = lock_map(&self.requests).get(request_id).cloned() else {
      return;
    };
    let error_text = params
      .get("errorText")
      .and_then(|v| v.as_str())
      .unwrap_or("net::ERR_FAILED")
      .to_string();
    req.set_failure(error_text.clone());
    let resp = lock_map(&self.responses).get(request_id).cloned();
    if let Some(resp) = resp {
      resp.finish_failure(error_text).await;
    }
    emitter.emit(crate::events::PageEvent::RequestFailed(req));
  }

  pub(super) fn on_websocket_created(
    self: &Arc<Self>,
    params: &serde_json::Value,
    emitter: &crate::events::EventEmitter,
  ) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let ws = WebSocket::new(url);
    lock_map(&self.websockets).insert(request_id.to_string(), ws.clone());
    emitter.emit(crate::events::PageEvent::WebSocket(ws));
  }

  pub(super) fn on_websocket_frame_sent(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let Some(payload) = crate::network::websocket_frame_payload(params) else {
      return;
    };
    if let Some(ws) = lock_map(&self.websockets).get(request_id) {
      ws.emit_frame_sent(payload);
    }
  }

  pub(super) fn on_websocket_frame_received(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let Some(payload) = crate::network::websocket_frame_payload(params) else {
      return;
    };
    if let Some(ws) = lock_map(&self.websockets).get(request_id) {
      ws.emit_frame_received(payload);
    }
  }

  pub(super) fn on_websocket_error(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let msg = params
      .get("errorMessage")
      .and_then(|v| v.as_str())
      .unwrap_or("WebSocket error")
      .to_string();
    if let Some(ws) = lock_map(&self.websockets).get(request_id) {
      ws.emit_error(msg);
    }
  }

  pub(super) fn on_websocket_closed(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
      return;
    };
    let ws = lock_map(&self.websockets).remove(request_id);
    if let Some(ws) = ws {
      ws.emit_close();
    }
  }

  fn build_response_from_value(
    self: &Arc<Self>,
    request: network::Request,
    resp: &serde_json::Value,
    request_id: &str,
  ) -> Response {
    let url = resp.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let status = resp.get("status").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let status_text = resp
      .get("statusText")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    let from_service_worker = resp
      .get("fromServiceWorker")
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false);
    let http_version = resp
      .get("protocol")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let headers = resp
      .get("headers")
      .and_then(|h| h.as_object())
      .map(|obj| {
        obj
          .iter()
          .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
          .collect::<Headers>()
      })
      .unwrap_or_default();
    let remote_addr = resp.get("remoteIPAddress").and_then(|v| v.as_str()).map(|ip| {
      let port = resp
        .get("remotePort")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(0);
      RemoteAddr {
        ip_address: ip.to_string(),
        port,
      }
    });
    let security = resp
      .get("securityDetails")
      .and_then(|s| s.as_object())
      .map(|obj| SecurityDetails {
        protocol: obj.get("protocol").and_then(|v| v.as_str()).map(String::from),
        subject_name: obj.get("subjectName").and_then(|v| v.as_str()).map(String::from),
        issuer: obj.get("issuer").and_then(|v| v.as_str()).map(String::from),
        valid_from: obj.get("validFrom").and_then(serde_json::Value::as_f64),
        valid_to: obj.get("validTo").and_then(serde_json::Value::as_f64),
      });
    let timing = resp.get("timing").map(parse_timing).unwrap_or_default();
    request.update_timing(timing);

    let body_fn = self.make_response_body_fn(request_id);
    let raw_headers_fn = provisional_raw_headers_fn(&headers);

    Response::new(ResponseInit {
      request,
      url,
      status,
      status_text,
      from_service_worker,
      http_version,
      headers,
      remote_addr,
      security_details: security,
      body_fn: Some(body_fn),
      raw_headers_fn,
    })
  }

  fn make_response_body_fn(self: &Arc<Self>, request_id: &str) -> BodyFn {
    let transport = Arc::clone(&self.transport);
    let session_id = self.session_id.clone();
    let request_id = request_id.to_string();
    Arc::new(move || {
      let transport = Arc::clone(&transport);
      let session_id = session_id.clone();
      let request_id = request_id.clone();
      Box::pin(async move {
        let resp = transport
          .send(
            session_id.as_deref(),
            &ferridriver_cdp_proto::network::GetResponseBody {
              request_id: &request_id,
            },
          )
          .await
          .map_err(|e| crate::error::FerriError::Protocol {
            method: "Network.getResponseBody".into(),
            message: e.to_string(),
          })?;
        if resp.base64_encoded {
          base64::engine::general_purpose::STANDARD
            .decode(&resp.body)
            .map_err(|e| crate::error::FerriError::Backend(format!("base64 decode: {e}")))
        } else {
          Ok(resp.body.into_bytes())
        }
      })
    })
  }
}

pub(super) fn parse_raw_headers(headers: Option<&serde_json::Value>) -> Vec<HeaderEntry> {
  let Some(headers) = headers else {
    return Vec::new();
  };
  let Some(obj) = headers.as_object() else {
    return Vec::new();
  };
  let mut out = Vec::with_capacity(obj.len());
  for (name, value) in obj {
    let raw = value.as_str().unwrap_or("");
    // CDP joins duplicate header values with `\n`; explode them back so
    // Playwright's `headersArray()` shape is preserved.
    for part in raw.split('\n') {
      out.push(HeaderEntry {
        name: name.clone(),
        value: part.to_string(),
      });
    }
  }
  out
}

pub(super) fn parse_timing(value: &serde_json::Value) -> RequestTiming {
  let f = |key: &str, default: f64| value.get(key).and_then(serde_json::Value::as_f64).unwrap_or(default);
  RequestTiming {
    // CDP `requestTime` is MONOTONIC seconds, not wall clock — the
    // request's epoch-ms anchor (from `requestWillBeSent.wallTime`)
    // survives the merge in `Request::update_timing`.
    start_time: 0.0,
    domain_lookup_start: f("dnsStart", -1.0),
    domain_lookup_end: f("dnsEnd", -1.0),
    connect_start: f("connectStart", -1.0),
    secure_connection_start: f("sslStart", -1.0),
    connect_end: f("connectEnd", -1.0),
    request_start: f("sendStart", -1.0),
    response_start: f("receiveHeadersStart", -1.0),
    response_end: f("receiveHeadersEnd", -1.0),
  }
}

/// Cancels a `WebMCP` invocation whose caller stopped waiting, so the page's
/// tool sees its signal abort instead of running on unobserved.
pub(super) struct PendingWebMcpInvocation {
  pub(super) transport: Arc<CdpTransport>,
  pub(super) session_id: Option<Arc<str>>,
  pub(super) id: Option<String>,
}

impl Drop for PendingWebMcpInvocation {
  fn drop(&mut self) {
    let Some(id) = self.id.take() else { return };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
      return;
    };
    let transport = Arc::clone(&self.transport);
    let session_id = self.session_id.clone();
    runtime.spawn(async move {
      if let Err(error) = transport
        .send(
          session_id.as_deref(),
          &ferridriver_cdp_proto::web_mcp::CancelInvocation { invocation_id: &id },
        )
        .await
      {
        tracing::debug!(%error, "WebMCP invocation could not be canceled");
      }
    });
  }
}

/// A cookie as `Storage.getCookies` / `Network.getCookies` return it.
pub(super) fn cookie_from_cdp(c: &serde_json::Value) -> CookieData {
  CookieData {
    name: c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    value: c.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    domain: c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    path: c.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    secure: c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false),
    http_only: c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false),
    expires: c.get("expires").and_then(serde_json::Value::as_f64),
    same_site: c
      .get("sameSite")
      .and_then(|v| v.as_str())
      .and_then(|v| v.parse::<super::super::SameSite>().ok()),
    url: None,
  }
}

/// A cookie as `Network.setCookie` and `Storage.setCookies` take it
/// (`Network.CookieParam`). Playwright's `SetNetworkCookieParam.url` is
/// forwarded: the browser derives domain and path from it, so
/// `addCookies([{ name, value, url }])` works without either.
pub(super) fn cookie_to_cdp(cookie: &CookieData) -> serde_json::Value {
  let mut params = serde_json::json!({
      "name": cookie.name,
      "value": cookie.value,
  });
  if let Some(u) = &cookie.url {
    params["url"] = serde_json::json!(u);
  }
  if !cookie.domain.is_empty() {
    params["domain"] = serde_json::json!(cookie.domain);
  }
  if !cookie.path.is_empty() {
    params["path"] = serde_json::json!(cookie.path);
  }
  params["secure"] = serde_json::json!(cookie.secure);
  params["httpOnly"] = serde_json::json!(cookie.http_only);
  if let Some(e) = cookie.expires {
    params["expires"] = serde_json::json!(e);
  }
  if let Some(ss) = cookie.same_site {
    params["sameSite"] = serde_json::json!(ss.as_str());
  }
  params
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::cdp::transport::CdpDispatcher;

  #[tokio::test]
  async fn tracker_drops_cleanly_without_arc_cycle_and_splits_multiline_provisional_headers() {
    let dispatcher = Arc::new(CdpDispatcher::new());
    let transport = Arc::new(CdpTransport::from_test_dispatcher(dispatcher));
    let tracker = Arc::new(NetworkTracker::new(
      transport,
      Some(Arc::from("sid-1")),
      Arc::from("target-1"),
      crate::network::NavRequestSlot::new(),
    ));
    let weak = Arc::downgrade(&tracker);
    let network_log = Arc::new(RwLock::new(Vec::new()));
    let emitter = crate::events::EventEmitter::new();

    tracker
      .on_request_will_be_sent(
        &serde_json::json!({
          "requestId": "req-1",
          "request": {
            "url": "https://example.test/api",
            "method": "GET",
            "headers": {
              "X-Multi": "one\ntwo",
              "Accept": "*/*"
            }
          },
          "type": "Fetch"
        }),
        &network_log,
        &emitter,
      )
      .await;
    tracker
      .on_response_received(
        &serde_json::json!({
          "requestId": "req-1",
          "response": {
            "url": "https://example.test/api",
            "status": 200,
            "statusText": "OK",
            "headers": {
              "Set-Cookie": "a=1\nb=2"
            }
          }
        }),
        &emitter,
      )
      .await;

    let req = network_log.read().await[0].clone();
    let req_headers = req.headers_array().await;
    let multi_vals: Vec<&str> = req_headers
      .iter()
      .filter(|h| h.name.eq_ignore_ascii_case("x-multi"))
      .map(|h| h.value.as_str())
      .collect();
    assert_eq!(multi_vals, vec!["one", "two"]);

    let resp = req.existing_response().await.expect("response attached");
    let cookies = resp.header_values("set-cookie").await.expect("response cookies");
    assert_eq!(cookies, vec!["a=1", "b=2"]);

    drop(tracker);
    assert!(
      weak.upgrade().is_none(),
      "NetworkTracker must not retain a strong Arc cycle through tracked requests/responses"
    );
  }
}
