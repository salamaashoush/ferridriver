//! `BidiNetworkTracker` — per-page live `Request`/`Response` bookkeeping and auth handling.

use super::super::{
  Arc, BodyFn, Engine, EventEmitter, FxHashMap, NetworkRequest, NetworkSettings, PageEvent, RemoteAddr, RequestInit,
  Response, ResponseInit, RwLock, SecurityDetails, json, network,
};
use super::route_actions::{auth_attempt_key, forget_auth_attempts};

/// Per-page bookkeeping for live `Request` / `Response` lifecycle objects
/// across the `BiDi` network event sequence (`beforeRequestSent` →
/// `responseStarted` → `responseCompleted` / `fetchError`).
///
/// W3C `BiDi` today does not expose `WebSocket` frame events — Playwright's
/// own `BiDi` backend skips `WebSocket` handling for the same reason — so
/// `WebSocket.body()` / `WebSocket` frame observation surface the typed
/// `FerriError::Unsupported` per Rule 4 instead of dangling indefinitely.
pub(super) struct BidiNetworkTracker {
  session: Arc<crate::backend::bidi::session::BidiSession>,
  /// Owning page's top-level browsing-context id — stamped on every
  /// captured `Request` as its `page_guid` for context-level page
  /// attribution.
  context_id: Arc<str>,
  requests: tokio::sync::Mutex<FxHashMap<String, NetworkRequest>>,
  responses: tokio::sync::Mutex<FxHashMap<String, Response>>,
  nav_request_slot: crate::network::NavRequestSlot,
  /// Shared with [`BidiPage::http_credentials`] — answers
  /// `network.authRequired` challenges.
  credentials: Arc<std::sync::Mutex<Option<Vec<crate::options::HttpCredentials>>>>,
  /// Shared with [`BidiPage::network`]; its proxy's credentials answer
  /// the proxy's challenges.
  network: Arc<std::sync::Mutex<NetworkSettings>>,
  /// Request ids already answered with `provideCredentials`. A repeat
  /// challenge for the same request means the credentials were wrong —
  /// cancel instead of looping (Playwright's
  /// `_attemptedAuthentications`). Cleared on request completion.
  attempted_auth: tokio::sync::Mutex<rustc_hash::FxHashSet<String>>,
}

impl BidiNetworkTracker {
  pub(super) fn new(
    session: Arc<crate::backend::bidi::session::BidiSession>,
    context_id: Arc<str>,
    nav_request_slot: crate::network::NavRequestSlot,
    credentials: Arc<std::sync::Mutex<Option<Vec<crate::options::HttpCredentials>>>>,
    network: Arc<std::sync::Mutex<NetworkSettings>>,
  ) -> Self {
    Self {
      session,
      context_id,
      requests: tokio::sync::Mutex::new(FxHashMap::default()),
      responses: tokio::sync::Mutex::new(FxHashMap::default()),
      nav_request_slot,
      credentials,
      network,
      attempted_auth: tokio::sync::Mutex::new(rustc_hash::FxHashSet::default()),
    }
  }

  pub(super) async fn on_before_request_sent(
    self: &Arc<Self>,
    params: &serde_json::Value,
    emitter: &EventEmitter,
    network_log: &Arc<RwLock<Vec<NetworkRequest>>>,
  ) {
    let Some(req) = params.get("request") else {
      return;
    };
    let id = req.get("request").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if id.is_empty() {
      return;
    }
    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET").to_string();
    let header_entries = req.get("headers").map(parse_bidi_header_entries).unwrap_or_default();
    let headers = crate::network::headers_array_to_map(&header_entries);
    let raw_headers_fn = crate::network::known_raw_headers(header_entries);
    let resource_type = resource_type_from_bidi(req, params).to_string();
    let body_size = req.get("bodySize").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let headers_size = req.get("headersSize").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let mut post_data = req
      .get("body")
      .and_then(|b| b.get("value"))
      .and_then(|v| v.as_str())
      .map(|s| s.as_bytes().to_vec());
    if post_data.is_none() && body_size > 0 {
      post_data = self.session.get_request_body(&id).await;
    }
    let frame_id = params
      .get("context")
      .and_then(|v| v.as_str())
      .map(std::string::ToString::to_string);
    let is_navigation_request = params.get("navigation").and_then(|v| v.as_str()).is_some();

    // Redirects: BiDi reuses the request id and signals the prior with
    // `redirectCount`. When the count > 0, treat it like the next hop.
    let redirected_from = if params
      .get("redirectCount")
      .and_then(serde_json::Value::as_u64)
      .unwrap_or(0)
      > 0
    {
      let mut requests = self.requests.lock().await;
      requests.remove(&id)
    } else {
      None
    };

    let new_request = network::Request::new(RequestInit {
      id: id.clone(),
      url,
      method,
      resource_type,
      is_navigation_request,
      post_data,
      headers,
      frame_id,
      page_guid: Some(self.context_id.to_string()),
      redirected_from,
      // BiDi event `timestamp` is epoch milliseconds.
      timing: params
        .get("timestamp")
        .and_then(serde_json::Value::as_f64)
        .filter(|ts| *ts > 0.0)
        .map(|ts| network::RequestTiming {
          start_time: ts,
          ..network::RequestTiming::empty()
        }),
      raw_headers_fn: Some(raw_headers_fn),
    });
    if headers_size > 0 || body_size > 0 {
      new_request.update_sizes(network::RequestSizes {
        request_body: body_size,
        request_headers: headers_size,
        response_body: 0,
        response_headers: 0,
      });
    }

    self.requests.lock().await.insert(id.clone(), new_request.clone());

    // Main-document navigations: update the per-page slot so
    // `BidiPage::goto` / `reload` / `go_back` / `go_forward` can
    // resolve the final main-document `Response` after the
    // lifecycle wait completes. BiDi flags a navigation request via
    // the `navigation` field on `network.beforeRequestSent`; the
    // slot therefore tracks each redirect hop (same request id,
    // reused across the chain). A child frame's document is a
    // navigation request of its own browsing context.
    if new_request.is_navigation_request() && new_request.frame_id() == Some(&*self.context_id) {
      self.nav_request_slot.set(new_request.clone());
    }

    crate::state::push_capped(
      &mut *network_log.write().await,
      new_request.clone(),
      crate::state::NETWORK_LOG_CAP,
    );
    emitter.emit(PageEvent::Request(new_request));
  }

  pub(super) async fn on_response_started(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
    let Some(request_id) = params
      .get("request")
      .and_then(|r| r.get("request"))
      .and_then(|v| v.as_str())
    else {
      return;
    };
    let request_id = request_id.to_string();
    let Some(req) = self.requests.lock().await.get(&request_id).cloned() else {
      return;
    };
    let Some(resp) = params.get("response") else {
      return;
    };
    let response = self.build_response(req.clone(), resp, &request_id);
    self.responses.lock().await.insert(request_id, response.clone());
    req.set_response(&response).await;
    emitter.emit(PageEvent::Response(response));
  }

  pub(super) async fn on_response_completed(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
    let Some(request_id) = params
      .get("request")
      .and_then(|r| r.get("request"))
      .and_then(|v| v.as_str())
    else {
      return;
    };
    let request_id = request_id.to_string();
    let Some(req) = self.requests.lock().await.get(&request_id).cloned() else {
      return;
    };
    // BiDi sometimes omits responseStarted (e.g. cache-served responses);
    // synthesise the Response here if necessary.
    if req.existing_response().await.is_none()
      && let Some(resp) = params.get("response")
    {
      let response = self.build_response(req.clone(), resp, &request_id);
      self.responses.lock().await.insert(request_id.clone(), response.clone());
      req.set_response(&response).await;
      emitter.emit(PageEvent::Response(response));
    }
    if let Some(resp) = params.get("response") {
      let resp_headers = resp.get("headersSize").and_then(serde_json::Value::as_u64).unwrap_or(0);
      let resp_body = resp
        .get("bytesReceived")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
      let req_headers = params
        .pointer("/request/headersSize")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
      let req_body = params
        .pointer("/request/bodySize")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
      if req_headers > 0 || req_body > 0 || resp_headers > 0 || resp_body > 0 {
        req.update_sizes(network::RequestSizes {
          request_body: req_body,
          request_headers: req_headers,
          response_body: resp_body,
          response_headers: resp_headers,
        });
      }
    }
    let response = self.responses.lock().await.get(&request_id).cloned();
    if let Some(resp) = response {
      resp.finish_success().await;
    }
    forget_auth_attempts(&mut *self.attempted_auth.lock().await, &request_id);
    emitter.emit(PageEvent::RequestFinished(req));
  }

  /// The session-wide `authRequired`-phase intercept pauses every auth
  /// challenge; this either answers it from the stored credentials or
  /// cancels it (surfacing the 401). Model:
  /// `bidiNetworkManager.ts::_onAuthRequired`. The
  /// `network.continueWithAuth` round-trip is spawned so the event
  /// listener never stalls behind it; a failure (e.g. the request died
  /// while the answer was in flight) is logged, matching Playwright's
  /// `sendMayFail`.
  pub(super) async fn on_auth_required(self: &Arc<Self>, params: &serde_json::Value) {
    let Some(request_id) = params
      .get("request")
      .and_then(|r| r.get("request"))
      .and_then(|v| v.as_str())
    else {
      return;
    };
    let is_basic = params
      .get("response")
      .and_then(|r| r.get("authChallenges"))
      .and_then(|v| v.as_array())
      .is_some_and(|challenges| {
        challenges.iter().any(|c| {
          c.get("scheme")
            .and_then(|s| s.as_str())
            .is_some_and(|s| s.starts_with("Basic"))
        })
      });
    let creds = self.credentials.lock().ok().and_then(|slot| slot.clone());
    let url = params
      .get("request")
      .and_then(|r| r.get("url"))
      .and_then(|v| v.as_str())
      .unwrap_or("");
    let matching = creds
      .as_deref()
      .and_then(|list| crate::options::matching_http_credentials(list, url));
    // Chromium reports neither the challenges nor reliably the status
    // over BiDi, so as in `bidiNetworkManager.ts` a challenge that is not
    // a plain 401 and matches no site credentials is taken for the proxy's.
    let status = params
      .get("response")
      .and_then(|r| r.get("status"))
      .and_then(serde_json::Value::as_u64)
      .unwrap_or(0);
    let from_proxy = status == 407 || (status != 401 && matching.is_none());
    let answering = if from_proxy {
      self.network.lock().ok().and_then(|network| {
        network
          .proxy
          .as_ref()
          .and_then(crate::options::ProxyConfig::credentials)
      })
    } else {
      matching.filter(|_| is_basic).cloned()
    };
    let answer = match answering {
      Some(c) => {
        let mut attempted = self.attempted_auth.lock().await;
        if attempted.insert(auth_attempt_key(request_id, from_proxy)) {
          json!({
            "request": request_id,
            "action": "provideCredentials",
            "credentials": {"type": "password", "username": c.username, "password": c.password},
          })
        } else {
          // Second challenge for the same request: the credentials
          // were rejected — cancel so the 401 surfaces instead of
          // retrying forever.
          json!({"request": request_id, "action": "cancel"})
        }
      },
      None => json!({"request": request_id, "action": "cancel"}),
    };
    let session = Arc::clone(&self.session);
    tracing::debug!(
      context = %self.context_id,
      request = request_id,
      event_context = ?params.get("context"),
      action = ?answer.get("action"),
      "answering HTTP authentication challenge"
    );
    tokio::spawn(async move {
      if let Err(e) = session.transport.send_command("network.continueWithAuth", answer).await {
        tracing::debug!("network.continueWithAuth failed: {e}");
      }
    });
  }

  pub(super) async fn on_fetch_error(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
    let Some(request_id) = params
      .get("request")
      .and_then(|r| r.get("request"))
      .and_then(|v| v.as_str())
    else {
      return;
    };
    let request_id = request_id.to_string();
    let Some(req) = self.requests.lock().await.get(&request_id).cloned() else {
      return;
    };
    let error_text = params
      .get("errorText")
      .and_then(|v| v.as_str())
      .unwrap_or("net::ERR_FAILED")
      .to_string();
    req.set_failure(error_text.clone());
    let response = self.responses.lock().await.get(&request_id).cloned();
    if let Some(resp) = response {
      resp.finish_failure(error_text).await;
    }
    forget_auth_attempts(&mut *self.attempted_auth.lock().await, &request_id);
    emitter.emit(PageEvent::RequestFailed(req));
  }

  fn build_response(self: &Arc<Self>, request: NetworkRequest, resp: &serde_json::Value, request_id: &str) -> Response {
    let url = resp.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let status = resp.get("status").and_then(serde_json::Value::as_i64).unwrap_or(0);
    let status_text = resp
      .get("statusText")
      .and_then(|v| v.as_str())
      .unwrap_or("")
      .to_string();
    let from_service_worker = resp
      .get("fromCache")
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false);
    let entries = resp.get("headers").map(parse_bidi_header_entries).unwrap_or_default();
    let headers = crate::network::headers_array_to_map(&entries);
    let body_fn = self.make_body_fn(request_id);
    let raw_headers_fn = crate::network::known_raw_headers(entries);
    Response::new(ResponseInit {
      request,
      url,
      status,
      status_text,
      from_service_worker,
      // BiDi `ResponseData.protocol` (e.g. "http/1.1") — same source as
      // Playwright's `_setHttpVersion(params.response.protocol)`.
      http_version: resp
        .get("protocol")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from),
      headers,
      remote_addr: parse_bidi_remote_addr(resp),
      security_details: parse_bidi_security_details(resp),
      body_fn: Some(body_fn),
      raw_headers_fn: Some(raw_headers_fn),
    })
  }

  fn make_body_fn(self: &Arc<Self>, request_id: &str) -> BodyFn {
    let session = Arc::clone(&self.session);
    let request_id = request_id.to_string();
    Arc::new(move || {
      let session = Arc::clone(&session);
      let request_id = request_id.clone();
      Box::pin(async move {
        // Bytes are retained by the session's `network.addDataCollector`
        // registration (connect-time, 20MB cap — bidiBrowser.ts:81);
        // without it Firefox discards them and this command errors with
        // "no such network data".
        let resp = session
          .transport
          .send_command(
            "network.getData",
            json!({"request": request_id, "dataType": "response"}),
          )
          .await
          .map_err(|e| crate::error::FerriError::Protocol {
            method: "network.getData".into(),
            message: e.to_string(),
          })?;
        let bytes = resp.get("bytes");
        let value = bytes
          .and_then(|b| b.get("value"))
          .and_then(|v| v.as_str())
          .unwrap_or("");
        // `bytes.type` is "string" (utf8 text) or "base64" — decode per
        // tag like Playwright's `getResponseBody`.
        match bytes.and_then(|b| b.get("type")).and_then(|v| v.as_str()) {
          Some("string") => Ok(value.as_bytes().to_vec()),
          _ => base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|e| crate::error::FerriError::Backend(format!("base64 decode: {e}"))),
        }
      })
    })
  }
}

/// Serialized origin (`scheme://host[:port]`) for permission grants and
/// credential-origin matching. Opaque origins (`data:`, `about:`,
/// unparsable URLs) return `None` — grants/credentials never apply to
/// them (Playwright skips `'null'` origins the same way).
pub(in crate::backend::bidi::page) fn permissions_origin(url: &str) -> Option<String> {
  let parsed = reqwest::Url::parse(url).ok()?;
  let origin = parsed.origin();
  if !origin.is_tuple() {
    return None;
  }
  Some(origin.ascii_serialization())
}

fn parse_bidi_remote_addr(_resp: &serde_json::Value) -> Option<RemoteAddr> {
  // WebDriver BiDi (W3C draft) does not currently surface remote IP /
  // port on `network.responseStarted` / `responseCompleted` payloads —
  // Playwright's own BiDi backend leaves the field unset for the same
  // reason. Returning `None` matches the other backend behaviours when
  // the field is missing rather than guessing.
  None
}

fn parse_bidi_security_details(resp: &serde_json::Value) -> Option<SecurityDetails> {
  // BiDi exposes `securityDetails` only for redirected responses on
  // some implementations; treat absence as None.
  resp
    .get("securityDetails")
    .and_then(|s| s.as_object())
    .map(|obj| SecurityDetails {
      protocol: obj.get("protocol").and_then(|v| v.as_str()).map(String::from),
      subject_name: obj.get("subjectName").and_then(|v| v.as_str()).map(String::from),
      issuer: obj.get("issuer").and_then(|v| v.as_str()).map(String::from),
      valid_from: obj.get("validFrom").and_then(serde_json::Value::as_f64),
      valid_to: obj.get("validTo").and_then(serde_json::Value::as_f64),
    })
}

/// Map `BiDi` `RequestData.destination` / `initiatorType` / `event.initiator.type`
/// into Playwright's canonical `ResourceType` vocabulary (mirrors
/// `bidiNetworkManager.ts::resourceTypeFromBidi`).
pub(in crate::backend::bidi::page) fn resource_type_from_bidi(
  req: &serde_json::Value,
  event: &serde_json::Value,
) -> &'static str {
  let destination = req.get("destination").and_then(serde_json::Value::as_str).unwrap_or("");
  match destination {
    "audio" | "video" => "media",
    "audioworklet" | "paintworklet" | "script" | "serviceworker" | "sharedworker" | "worker" => "script",
    "document" | "frame" | "iframe" => "document",
    "font" => "font",
    "image" => "image",
    "style" => "stylesheet",
    "track" => "texttrack",
    "" => {
      let req_initiator = req.get("initiatorType").and_then(serde_json::Value::as_str);
      match req_initiator {
        Some("fetch") => "fetch",
        Some("font") => "font",
        Some("xmlhttprequest") => "xhr",
        None => match event.pointer("/initiator/type").and_then(serde_json::Value::as_str) {
          Some("script") => "xhr",
          Some("preflight") => "preflight",
          _ => "document",
        },
        Some(_) => "other",
      }
    },
    _ => "other",
  }
}

/// Parse BiDi-format headers `[{name, value: {type, value}}]` into a `FxHashMap`.
pub(in crate::backend::bidi::page) fn parse_bidi_headers(headers_val: &serde_json::Value) -> FxHashMap<String, String> {
  crate::network::headers_array_to_map(&parse_bidi_header_entries(headers_val))
}

/// `BiDi` reports headers as an ARRAY, so a header sent twice arrives
/// as two entries and nothing has to be un-joined to see them. Reading
/// it straight into a map is what used to lose them.
pub(super) fn parse_bidi_header_entries(headers_val: &serde_json::Value) -> Vec<crate::network::HeaderEntry> {
  headers_val
    .as_array()
    .map(|arr| {
      arr
        .iter()
        .filter_map(|entry| {
          Some(crate::network::HeaderEntry {
            name: entry.get("name")?.as_str()?.to_string(),
            value: entry
              .get("value")
              .and_then(|v| v.get("value"))
              .and_then(|v| v.as_str())
              .unwrap_or("")
              .to_string(),
          })
        })
        .collect()
    })
    .unwrap_or_default()
}
