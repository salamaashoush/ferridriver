//! PW `WebKit` event translation: protocol events → ferridriver
//! [`ConsoleMessage`] / [`DialogEvent`] / accessibility / cookie types.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio::sync::broadcast::error::RecvError;

use super::connection::Session;
use super::page::WebKitPage;
use crate::backend::{AxNodeData, AxProperty, CookieData, SameSite};
use crate::console_message::{ConsoleMessage, ConsoleMessageLocation};
use crate::context::DialogEvent;
use crate::error::{FerriError, Result};
use crate::network::{
  BodyFn, Headers, RemoteAddr, Request as NetworkRequest, RequestInit, Response, ResponseInit, SecurityDetails,
  WebSocket, WebSocketPayload,
};

/// Parse the JSON array produced by `window.__fd.accessibilityTree(depth)`
/// into [`AxNodeData`]. Shared shape with the `BiDi` backend.
#[must_use]
pub fn parse_ax_nodes(arr: &[Value]) -> Vec<AxNodeData> {
  let mut nodes = Vec::with_capacity(arr.len());
  for item in arr {
    let mut properties = Vec::new();
    let mut push_str = |name: &str, key: &str| {
      if let Some(v) = item.get(key).and_then(Value::as_str) {
        if !v.is_empty() {
          properties.push(AxProperty {
            name: name.to_string(),
            value: Some(Value::String(v.to_string())),
          });
        }
      }
    };
    push_str("checked", "checked");
    push_str("expanded", "expanded");
    push_str("url", "url");
    for (name, key) in [
      ("disabled", "disabled"),
      ("readonly", "readonly"),
      ("required", "required"),
    ] {
      if item.get(key).and_then(Value::as_bool).unwrap_or(false) {
        properties.push(AxProperty {
          name: name.to_string(),
          value: Some(Value::Bool(true)),
        });
      }
    }
    if let Some(level) = item.get("level").and_then(Value::as_i64).filter(|l| *l > 0) {
      properties.push(AxProperty {
        name: "level".to_string(),
        value: Some(json!(level)),
      });
    }
    nodes.push(AxNodeData {
      node_id: item.get("nodeId").and_then(Value::as_str).unwrap_or("").to_string(),
      parent_id: item.get("parentId").and_then(Value::as_str).map(String::from),
      backend_dom_node_id: item.get("backendId").and_then(Value::as_i64),
      ignored: item.get("ignored").and_then(Value::as_bool).unwrap_or(false),
      role: item.get("role").and_then(Value::as_str).map(String::from),
      name: item.get("name").and_then(Value::as_str).map(String::from),
      description: item.get("description").and_then(Value::as_str).map(String::from),
      properties,
    });
  }
  nodes
}

/// Parse one `Page.Cookie` JSON object into [`CookieData`].
#[must_use]
pub fn parse_cookie(c: &Value) -> CookieData {
  let same_site = c.get("sameSite").and_then(Value::as_str).and_then(|s| match s {
    "Strict" => Some(SameSite::Strict),
    "Lax" => Some(SameSite::Lax),
    "None" => Some(SameSite::None),
    _ => None,
  });
  CookieData {
    name: c.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
    value: c.get("value").and_then(Value::as_str).unwrap_or("").to_string(),
    domain: c.get("domain").and_then(Value::as_str).unwrap_or("").to_string(),
    path: c.get("path").and_then(Value::as_str).unwrap_or("/").to_string(),
    secure: c.get("secure").and_then(Value::as_bool).unwrap_or(false),
    http_only: c.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
    expires: c.get("expires").and_then(Value::as_f64),
    same_site,
    url: None,
  }
}

/// `Page.setCookie` for the PW `WebKit` backend.
pub async fn set_cookie(target: &Session, cookie: CookieData) -> Result<()> {
  let mut obj = json!({
    "name": cookie.name,
    "value": cookie.value,
    "domain": cookie.domain,
    "path": if cookie.path.is_empty() { "/".to_string() } else { cookie.path },
    "secure": cookie.secure,
    "httpOnly": cookie.http_only,
    "session": cookie.expires.is_none(),
  });
  if let Some(exp) = cookie.expires {
    obj["expires"] = json!(exp);
  }
  obj["sameSite"] = json!(cookie.same_site.map_or("None", SameSite::as_str));
  // PW WebKit `Page.setCookie` derives the URL from domain+path+secure
  // when no explicit url is given.
  let scheme = if cookie.secure { "https" } else { "http" };
  obj["url"] = json!(format!("{scheme}://{}", obj["domain"].as_str().unwrap_or("")));
  target
    .send("Page.setCookie", json!({ "cookie": obj }))
    .await
    .map_err(|e| FerriError::backend(format!("webkit set_cookie: {e}")))?;
  Ok(())
}

/// Spawn the always-on per-page listener loop. Translates `Console.*` /
/// `Network.*` / `Dialog.*` / frame / route / websocket events into
/// page-level state (and writes to the page's `ArcSwap`-held console /
/// network / dialog logs), feeds `super::page::LifecycleSignals` for
/// `wait_for_lifecycle`, and handles `Target.*` cross-process swaps
/// via the provisional-target slot.
///
/// Called from [`super::page::WebKitPage::attach`] — no separate
/// "wire it up later" step. [`super::page::WebKitPage::attach_listeners`]
/// only swaps in the caller's log sinks via the `ArcSwap` fields.
pub fn attach_listeners(page: &WebKitPage) {
  let mut target_rx = page.target_session().events();
  let mut proxy_rx = page.proxy_session().events();
  let proxy = page.proxy_session().clone();
  let dialog_manager = page.dialog_manager.clone();
  let file_chooser_manager = page.file_chooser_manager.clone();
  let page_backref = page.page_backref.clone();
  let emitter = page.events.clone();
  let requests = page.requests.clone();
  let nav_slot = page.nav_request_slot.clone();
  let routes = page.routes.clone();
  let frame_contexts = page.frame_contexts.clone();
  let frame_cache = page.frame_cache.clone();
  let websockets = page.websockets.clone();
  let emitter_frame = emitter.clone();
  let _ = dialog_manager.register_emitter_bridge(emitter.clone());

  let ctx = TargetListenerCtx {
    target_swap: page.target_swap(),
    emitter,
    emitter_frame,
    page_backref,
    file_chooser_manager,
    requests,
    nav_slot,
    routes,
    frame_contexts,
    frame_cache,
    websockets,
    console_log: Arc::clone(&page.console_log),
    network_log: Arc::clone(&page.network_log),
    lifecycle: Arc::clone(&page.lifecycle),
    main_frame_id_cache: Arc::clone(&page.main_frame_id_cache),
  };
  let dialog_log = Arc::clone(&page.dialog_log);
  // Provisional-target slot. Populated on `Target.targetCreated` with
  // `isProvisional: true`; consumed on `Target.didCommitProvisionalTarget`
  // to swap the page's live target session.
  let provisional: ProvisionalSlot = Arc::new(tokio::sync::Mutex::new(None));
  let page = page.clone();
  tokio::spawn(async move {
    loop {
      tokio::select! {
        ev = target_rx.recv() => match ev {
          Ok(env) => dispatch_target_event(&ctx, env).await,
          Err(RecvError::Lagged(_)) => {},
          Err(RecvError::Closed) => break,
        },
        ev = proxy_rx.recv() => match ev {
          Ok(env) => match env.method.as_deref() {
            Some("Dialog.javascriptDialogOpening") => {
              let log = arc_swap::Guard::into_inner(dialog_log.load());
              dispatch_dialog(&proxy, &env.params, &dialog_manager, &log, page.page_backref.weak()).await;
            },
            Some("Target.targetCreated") => {
              handle_provisional_target_created(&env.params, &page, provisional.clone()).await;
            },
            Some("Target.didCommitProvisionalTarget") => {
              if let Some(new_rx) =
                handle_committed_provisional_target(&env.params, &page, provisional.clone()).await
              {
                target_rx = new_rx;
              }
            },
            _ => {},
          },
          Err(RecvError::Lagged(_)) => {},
          Err(RecvError::Closed) => break,
        },
      }
    }
  });
}

/// Handle `Target.targetCreated` with `isProvisional: true` — open a
/// fresh target session for the new (post-process-swap) page, run the
/// standard `*.enable` initialisation, apply per-page context
/// overrides, then `Target.resume` the paused new process. Stashes the
/// new session in `provisional` so the matching
/// `Target.didCommitProvisionalTarget` event can complete the swap.
///
/// Mirrors `wkPage.ts::_onTargetCreated` for the `isProvisional: true`
/// branch, which constructs a `WKProvisionalPage`, opens a session,
/// and resumes the paused target.
async fn handle_provisional_target_created(params: &Value, page: &WebKitPage, provisional: ProvisionalSlot) {
  let Some(info) = params.get("targetInfo") else {
    return;
  };
  if info.get("type").and_then(Value::as_str) != Some("page") {
    return;
  }
  if !info.get("isProvisional").and_then(Value::as_bool).unwrap_or(false) {
    return;
  }
  let target_id = match info.get("targetId").and_then(Value::as_str) {
    Some(s) => s.to_string(),
    None => return,
  };
  let proxy = page.proxy_session().clone();
  let proxy_id = page.page_proxy_id().to_string();
  let conn = proxy.connection_handle();
  let new_target = conn.target_session(proxy_id, target_id.clone());
  // *.enable, mirroring `WKPage._initializeSessionMayThrow`.
  let _ = new_target.send("Page.enable", json!({})).await;
  let _ = new_target.send("Runtime.enable", json!({})).await;
  let _ = new_target.send("Network.enable", json!({})).await;
  let _ = new_target.send("Console.enable", json!({})).await;
  // Re-apply extra HTTP headers on the new target — WebKit drops them on
  // the session swap, so a `setExtraHTTPHeaders` issued before this
  // navigation would otherwise not reach the request (mirrors
  // Playwright's `_updateState` re-application).
  let extra_headers = page
    .extra_http_headers
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  if let Some(headers) = extra_headers {
    let _ = new_target
      .send("Network.setExtraHTTPHeaders", json!({ "headers": headers }))
      .await;
  }
  // Re-apply media / user-preference emulation the same way — the new
  // target session starts with no overrides, so a prior `emulateMedia`
  // would otherwise stop holding after the first navigation.
  let emulated = page
    .emulated_media
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  if let Some(opts) = emulated {
    for (method, params) in super::page::WebKitPage::emulate_media_commands(&opts) {
      let _ = new_target.send(method, params).await;
    }
  }
  let _ = new_target
    .send(
      "Page.createUserWorld",
      json!({ "name": super::page::UTILITY_WORLD_NAME }),
    )
    .await;
  // Stash before resuming — a fast commit could fire before `await`
  // releases here, and the swap reader needs to find the session.
  {
    let mut slot = provisional.lock().await;
    *slot = Some((new_target, Arc::<str>::from(target_id.clone())));
  }
  if info.get("isPaused").and_then(Value::as_bool).unwrap_or(false) {
    let _ = proxy.send("Target.resume", json!({ "targetId": target_id })).await;
  }
}

/// Handle `Target.didCommitProvisionalTarget` — atomically swap the
/// page's live target session to the previously-stashed provisional
/// session and return a fresh `target_rx` for the new session so the
/// listener loop starts seeing events from the new process.
///
/// Mirrors `wkPage.ts::_onDidCommitProvisionalTarget` which calls
/// `_setSession(newSession)`.
async fn handle_committed_provisional_target(
  params: &Value,
  page: &WebKitPage,
  provisional: ProvisionalSlot,
) -> Option<tokio::sync::broadcast::Receiver<super::protocol::Envelope>> {
  let new_target_id = params.get("newTargetId").and_then(Value::as_str)?.to_string();
  let (new_session, stashed_id) = provisional.lock().await.take()?;
  if &*stashed_id != new_target_id.as_str() {
    // Defensive: if WebKit committed a target other than the one we
    // stashed, drop the stash and let the next attach cycle recover.
    return None;
  }
  let new_rx = new_session.events();
  page.swap_target_session(new_session, stashed_id);
  Some(new_rx)
}

/// Bundle of per-page handles + state the target listener loop hands
/// to its event-dispatch helper. Extracted from `attach_listeners` to
/// keep the listener function inside clippy's 100-line cap; nothing
/// here is borrow-cheap enough to inline at every event site.
///
/// `target_swap` holds the live target session — read fresh on every
/// dispatch so handlers always send on the current session, even after
/// a provisional-target commit swap (cross-process navigation).
struct TargetListenerCtx {
  target_swap: Arc<arc_swap::ArcSwap<super::connection::Session>>,
  emitter: crate::events::EventEmitter,
  emitter_frame: crate::events::EventEmitter,
  page_backref: crate::backend::PageBackref,
  file_chooser_manager: crate::file_chooser::FileChooserManager,
  requests: Requests,
  nav_slot: crate::network::NavRequestSlot,
  routes: Routes,
  frame_contexts: FrameContexts,
  frame_cache: FrameCache,
  websockets: WebSockets,
  /// Sinks for captured events. Swapped in by
  /// [`super::page::WebKitPage::attach_listeners`]; the listener
  /// reads the current pointer on each event so post-attach calls land
  /// in the caller's logs.
  console_log: Arc<arc_swap::ArcSwap<RwLock<Vec<ConsoleMessage>>>>,
  network_log: Arc<arc_swap::ArcSwap<RwLock<Vec<NetworkRequest>>>>,
  lifecycle: Arc<super::page::LifecycleSignals>,
  main_frame_id_cache: Arc<std::sync::Mutex<Option<String>>>,
}

impl TargetListenerCtx {
  fn target(&self) -> super::connection::Session {
    super::connection::Session::clone(&self.target_swap.load())
  }
}

async fn dispatch_target_event(ctx: &TargetListenerCtx, env: super::protocol::Envelope) {
  match env.method.as_deref() {
    Some("Console.messageAdded") => {
      let log = arc_swap::Guard::into_inner(ctx.console_log.load());
      dispatch_console(&env.params, &log, &ctx.emitter, &ctx.page_backref).await;
    },
    Some("Network.requestWillBeSent") => {
      let log = arc_swap::Guard::into_inner(ctx.network_log.load());
      handle_request_will_be_sent(&env.params, &ctx.requests, &ctx.nav_slot, &log, &ctx.emitter).await;
    },
    Some("Network.responseReceived") => {
      let target = ctx.target();
      handle_response_received(&env.params, &ctx.requests, &target, &ctx.emitter).await;
    },
    Some("Network.loadingFinished") => {
      handle_loading_finished(&env.params, &ctx.requests, &ctx.emitter);
    },
    Some("Network.loadingFailed") => {
      if let (Some(request_id), error_text) = (
        env.params.get("requestId").and_then(Value::as_str),
        env
          .params
          .get("errorText")
          .and_then(Value::as_str)
          .unwrap_or("navigation failed"),
      ) {
        ctx
          .lifecycle
          .mark_failed(request_id.to_string(), error_text.to_string());
      }
      handle_loading_failed(&env.params, &ctx.requests, &ctx.emitter);
    },
    Some("Page.loadEventFired") => {
      ctx.lifecycle.mark(crate::backend::NavLifecycle::Load);
      ctx.emitter.emit(crate::events::PageEvent::Load);
    },
    Some("Page.domContentEventFired") => {
      ctx.lifecycle.mark(crate::backend::NavLifecycle::DomContentLoaded);
      ctx.emitter.emit(crate::events::PageEvent::DomContentLoaded);
    },
    Some("Page.fileChooserOpened") => {
      let target = ctx.target();
      dispatch_file_chooser(&env.params, &target, &ctx.page_backref, &ctx.file_chooser_manager);
    },
    Some("Network.requestIntercepted") => {
      let target = ctx.target();
      handle_request_intercepted(&env.params, &target, &ctx.routes);
    },
    Some("Runtime.executionContextCreated") => {
      handle_exec_context_created(&env.params, &ctx.frame_contexts).await;
    },
    Some("Page.frameAttached") => {
      handle_frame_attached(&env.params, &ctx.frame_cache, &ctx.emitter_frame);
    },
    Some("Page.frameNavigated") => {
      // Only main-document commits feed the lifecycle latch — child
      // frame commits would falsely mark navigation as complete on the
      // page level. Also re-seed the main-frame-id cache: a
      // cross-process target swap cleared it, and the new target's
      // first main-frame commit gives us the new id.
      if env.params.get("frame").and_then(|f| f.get("parentId")).is_none() {
        ctx.lifecycle.mark(crate::backend::NavLifecycle::Commit);
        if let Some(new_main_id) = env
          .params
          .get("frame")
          .and_then(|f| f.get("id"))
          .and_then(Value::as_str)
        {
          let mut slot = ctx
            .main_frame_id_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
          *slot = Some(new_main_id.to_string());
        }
      }
      handle_frame_navigated(&env.params, &ctx.frame_cache, &ctx.emitter_frame);
    },
    Some("Page.frameDetached") => {
      handle_frame_detached(&env.params, &ctx.frame_cache, &ctx.emitter_frame, &ctx.frame_contexts).await;
    },
    Some("Network.webSocketCreated") => handle_websocket_created(&env.params, &ctx.websockets, &ctx.emitter).await,
    Some("Network.webSocketFrameSent") => handle_websocket_frame(&env.params, &ctx.websockets, true).await,
    Some("Network.webSocketFrameReceived") => handle_websocket_frame(&env.params, &ctx.websockets, false).await,
    Some("Network.webSocketFrameError") => handle_websocket_error(&env.params, &ctx.websockets).await,
    Some("Network.webSocketClosed") => handle_websocket_closed(&env.params, &ctx.websockets).await,
    _ => {},
  }
}

type Requests = Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, NetworkRequest>>>;
type ProvisionalSlot = Arc<tokio::sync::Mutex<Option<(super::connection::Session, Arc<str>)>>>;

/// Push a fresh [`NetworkRequest`] into the per-page table and the
/// context log. When `redirectResponse` is present, link the chain
/// (the prior request's `redirected_to` slot points at the new one).
async fn handle_request_will_be_sent(
  params: &Value,
  requests: &Requests,
  nav_slot: &crate::network::NavRequestSlot,
  network_log: &Arc<RwLock<Vec<NetworkRequest>>>,
  emitter: &crate::events::EventEmitter,
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
  let mut headers = Headers::default();
  if let Some(map) = request_payload.get("headers").and_then(Value::as_object) {
    for (k, v) in map {
      if let Some(s) = v.as_str() {
        headers.insert(k.clone(), s.to_string());
      }
    }
  }
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
    redirected_from,
    timing: None,
    raw_headers_fn: None,
  });
  requests
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .insert(request_id, req.clone());
  network_log.write().await.push(req.clone());
  if is_navigation_request {
    nav_slot.set(req.clone());
  }
  emitter.emit(crate::events::PageEvent::Request(req));
}

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
  let mut headers = Headers::default();
  if let Some(map) = response.get("headers").and_then(Value::as_object) {
    for (k, v) in map {
      if let Some(s) = v.as_str() {
        headers.insert(k.clone(), s.to_string());
      }
    }
  }
  let remote_addr = response
    .get("remoteIPAddress")
    .and_then(Value::as_str)
    .map(|ip| RemoteAddr {
      ip_address: ip.to_string(),
      port: u16::try_from(response.get("remotePort").and_then(Value::as_u64).unwrap_or(0)).unwrap_or(0),
    });
  let security_details: Option<SecurityDetails> = response.get("security").map(|s| SecurityDetails {
    protocol: s.get("protocol").and_then(Value::as_str).map(String::from),
    subject_name: s.get("subjectName").and_then(Value::as_str).map(String::from),
    issuer: s.get("issuer").and_then(Value::as_str).map(String::from),
    valid_from: s.get("validFrom").and_then(Value::as_f64),
    valid_to: s.get("validTo").and_then(Value::as_f64),
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
    raw_headers_fn: None,
  })
}

async fn handle_response_received(
  params: &Value,
  requests: &Requests,
  target: &super::connection::Session,
  emitter: &crate::events::EventEmitter,
) {
  let request_id = params
    .get("requestId")
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_string();
  let Some(req) = requests
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .get(&request_id)
    .cloned()
  else {
    return;
  };
  let Some(response_payload) = params.get("response") else {
    return;
  };
  let body_fn = build_body_fn(target.clone(), request_id);
  let response = build_response(req.clone(), response_payload, Some(body_fn));
  req.set_response(&response).await;
  emitter.emit(crate::events::PageEvent::Response(response));
}

/// Build a body-fetcher closure that runs `Network.getResponseBody`
/// against the given target session.
fn build_body_fn(target: super::connection::Session, request_id: String) -> BodyFn {
  use base64::Engine as _;
  Arc::new(move || {
    let target = target.clone();
    let request_id = request_id.clone();
    Box::pin(async move {
      let resp = target
        .send("Network.getResponseBody", json!({ "requestId": request_id }))
        .await
        .map_err(|e| crate::error::FerriError::backend(format!("getResponseBody: {e}")))?;
      let body = resp.get("body").and_then(Value::as_str).unwrap_or("");
      let base64 = resp.get("base64Encoded").and_then(Value::as_bool).unwrap_or(false);
      if base64 {
        base64::engine::general_purpose::STANDARD
          .decode(body)
          .map_err(|e| crate::error::FerriError::backend(format!("getResponseBody base64: {e}")))
      } else {
        Ok(body.as_bytes().to_vec())
      }
    })
  })
}

fn handle_loading_finished(params: &Value, requests: &Requests, emitter: &crate::events::EventEmitter) {
  let request_id = params.get("requestId").and_then(Value::as_str).unwrap_or("");
  let req = requests
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .remove(request_id);
  if let Some(req) = req {
    emitter.emit(crate::events::PageEvent::RequestFinished(req));
  }
}

fn handle_loading_failed(params: &Value, requests: &Requests, emitter: &crate::events::EventEmitter) {
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
    req.set_failure(error_text);
    emitter.emit(crate::events::PageEvent::RequestFailed(req));
  }
}

/// Convert one PW `WebKit` `Runtime.RemoteObject` JSON payload into a
/// [`crate::js_handle::JSHandleBacking`]. Object-id-bearing objects
/// become remote-backed handles; primitives ride back as
/// [`crate::js_handle::JSHandleBacking::Value`].
fn pw_remote_object_to_backing(arg: &Value) -> crate::js_handle::JSHandleBacking {
  use crate::js_handle::{HandleRemote, JSHandleBacking};
  use crate::protocol::{SerializationContext, SerializedValue, SpecialValue};

  if let Some(obj_id) = arg.get("objectId").and_then(Value::as_str) {
    return JSHandleBacking::Remote(HandleRemote::WebKit(Arc::from(obj_id)));
  }
  let value = arg.get("value").cloned().unwrap_or(Value::Null);
  let ty = arg.get("type").and_then(Value::as_str).unwrap_or("");
  let serialized = if value.is_null() {
    if ty == "undefined" {
      SerializedValue::Special(SpecialValue::Undefined)
    } else {
      SerializedValue::Special(SpecialValue::Null)
    }
  } else {
    SerializedValue::from_json(&value, &mut SerializationContext::default())
  };
  JSHandleBacking::Value(serialized)
}

/// Build a [`ConsoleMessage`] from a `Console.messageAdded` payload
/// and dispatch it through both the per-context log and the page's
/// event emitter. Drops the event when no outer `Arc<Page>` is
/// reachable through `page_backref` — matches Playwright's
/// `createHandle(context, arg)` guard.
type Routes = Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>;
type FrameContexts = Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>;
type FrameCache = Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>;

async fn handle_exec_context_created(params: &Value, frame_contexts: &FrameContexts) {
  let Some(ctx) = params.get("context") else {
    return;
  };
  let Some(frame_id) = ctx.get("frameId").and_then(Value::as_str) else {
    return;
  };
  let Some(id) = ctx.get("id").and_then(Value::as_i64) else {
    return;
  };
  frame_contexts.write().await.insert(frame_id.to_string(), id);
}

fn handle_frame_attached(params: &Value, frame_cache: &FrameCache, emitter: &crate::events::EventEmitter) {
  let Some(frame_id) = params.get("frameId").and_then(Value::as_str) else {
    return;
  };
  let parent_id = params
    .get("parentFrameId")
    .and_then(Value::as_str)
    .map(std::string::ToString::to_string);
  if let Ok(mut cache) = frame_cache.lock() {
    cache.attach(crate::backend::FrameInfo {
      frame_id: frame_id.to_string(),
      parent_frame_id: parent_id.clone(),
      name: String::new(),
      url: String::new(),
    });
  }
  emitter.emit(crate::events::PageEvent::FrameAttached(crate::backend::FrameInfo {
    frame_id: frame_id.to_string(),
    parent_frame_id: parent_id,
    name: String::new(),
    url: String::new(),
  }));
}

fn handle_frame_navigated(params: &Value, frame_cache: &FrameCache, emitter: &crate::events::EventEmitter) {
  let Some(frame) = params.get("frame") else {
    return;
  };
  let info = crate::backend::FrameInfo {
    frame_id: frame.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
    parent_frame_id: frame
      .get("parentId")
      .and_then(Value::as_str)
      .map(std::string::ToString::to_string),
    name: frame.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
    url: frame.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
  };
  if let Ok(mut cache) = frame_cache.lock() {
    cache.navigated(info.clone());
  }
  emitter.emit(crate::events::PageEvent::FrameNavigated(info));
}

async fn handle_frame_detached(
  params: &Value,
  frame_cache: &FrameCache,
  emitter: &crate::events::EventEmitter,
  frame_contexts: &FrameContexts,
) {
  let Some(frame_id) = params.get("frameId").and_then(Value::as_str) else {
    return;
  };
  if let Ok(mut cache) = frame_cache.lock() {
    cache.detach(frame_id);
  }
  frame_contexts.write().await.remove(frame_id);
  emitter.emit(crate::events::PageEvent::FrameDetached {
    frame_id: frame_id.to_string(),
  });
}

type WebSockets = Arc<tokio::sync::Mutex<rustc_hash::FxHashMap<String, WebSocket>>>;

/// `Network.webSocketCreated` → register live [`WebSocket`] keyed by
/// `requestId` and emit [`PageEvent::WebSocket`] so user code attached
/// via `page.waitForEvent('websocket')` can grab it.
async fn handle_websocket_created(params: &Value, websockets: &WebSockets, emitter: &crate::events::EventEmitter) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let url = params.get("url").and_then(Value::as_str).unwrap_or("").to_string();
  let ws = WebSocket::new(url);
  websockets.lock().await.insert(request_id.to_string(), ws.clone());
  emitter.emit(crate::events::PageEvent::WebSocket(ws));
}

/// `Network.webSocketFrame{Sent,Received}` → emit one frame on the live
/// [`WebSocket`]. Opcode 2 → binary (base64-decoded), otherwise text.
async fn handle_websocket_frame(params: &Value, websockets: &WebSockets, sent: bool) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let payload = parse_websocket_frame(params);
  let map = websockets.lock().await;
  if let Some(ws) = map.get(request_id) {
    if sent {
      ws.emit_frame_sent(payload);
    } else {
      ws.emit_frame_received(payload);
    }
  }
}

async fn handle_websocket_error(params: &Value, websockets: &WebSockets) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  let message = params
    .get("errorMessage")
    .and_then(Value::as_str)
    .unwrap_or("WebSocket error")
    .to_string();
  if let Some(ws) = websockets.lock().await.get(request_id) {
    ws.emit_error(message);
  }
}

async fn handle_websocket_closed(params: &Value, websockets: &WebSockets) {
  let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
    return;
  };
  if let Some(ws) = websockets.lock().await.remove(request_id) {
    ws.emit_close();
  }
}

/// Decode `WebSocketFrame` payload — opcode 2 = binary (base64).
fn parse_websocket_frame(params: &Value) -> WebSocketPayload {
  use base64::Engine as _;
  let response = params.get("response");
  let opcode = response
    .and_then(|r| r.get("opcode"))
    .and_then(Value::as_u64)
    .unwrap_or(1);
  let payload_data = response
    .and_then(|r| r.get("payloadData"))
    .and_then(Value::as_str)
    .unwrap_or("");
  if opcode == 2 {
    let bytes = base64::engine::general_purpose::STANDARD
      .decode(payload_data)
      .unwrap_or_else(|_| payload_data.as_bytes().to_vec());
    WebSocketPayload::Binary(bytes)
  } else {
    WebSocketPayload::Text(payload_data.to_string())
  }
}

/// Translate `Network.requestIntercepted` into a matched
/// [`crate::route::Route`] dispatch. When no route matches, continue
/// unmodified.
fn handle_request_intercepted(params: &Value, target: &super::connection::Session, routes: &Routes) {
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
  let routes = routes.clone();
  tokio::spawn(async move { dispatch_intercepted(target, routes, request_id, request_payload).await });
}

async fn dispatch_intercepted(
  target: super::connection::Session,
  routes: Routes,
  request_id: String,
  request_payload: Value,
) {
  let intercepted = build_intercepted(&request_id, &request_payload);
  let handler = {
    let mut guard = routes.write().await;
    crate::route::take_matching_handler(&mut guard, &intercepted.url)
  };
  let Some(handler) = handler else {
    let _ = target
      .send(
        "Network.interceptContinue",
        json!({ "requestId": request_id, "stage": "request" }),
      )
      .await;
    return;
  };
  let (action_tx, action_rx) = tokio::sync::oneshot::channel();
  let route = crate::route::Route::new(intercepted, action_tx);
  handler(route);
  let action = action_rx.await.unwrap_or(crate::route::RouteAction::Continue(
    crate::route::ContinueOverrides::default(),
  ));
  match action {
    crate::route::RouteAction::Continue(overrides) => intercept_continue(&target, &request_id, overrides).await,
    crate::route::RouteAction::Fulfill(response) => intercept_fulfill(&target, &request_id, &response).await,
    crate::route::RouteAction::Abort(_) => {
      let _ = target
        .send(
          "Network.interceptRequestWithError",
          json!({ "requestId": request_id, "errorType": "Cancellation" }),
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
  let post_data = request_payload
    .get("postData")
    .and_then(Value::as_str)
    .map(std::string::ToString::to_string);
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
  target: &super::connection::Session,
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

async fn intercept_fulfill(
  target: &super::connection::Session,
  request_id: &str,
  response: &crate::route::FulfillResponse,
) {
  use base64::Engine as _;
  let mut headers_map = serde_json::Map::new();
  let mut mime_type = String::from("text/plain");
  for (k, v) in &response.headers {
    if k.eq_ignore_ascii_case("content-type") {
      mime_type = v.clone();
    }
    headers_map.insert(k.clone(), Value::String(v.clone()));
  }
  if let Some(ct) = response.content_type.as_ref() {
    mime_type = ct.clone();
    headers_map.insert("content-type".to_string(), Value::String(ct.clone()));
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
fn dispatch_file_chooser(
  params: &Value,
  target: &super::connection::Session,
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
    let element = super::element::WebKitElement::new(target, object_id);
    let any_element = crate::backend::AnyElement::WebKit(element);
    let Ok(handle) = crate::element_handle::ElementHandle::from_any_element(page, any_element).await else {
      return;
    };
    let chooser = crate::file_chooser::FileChooser::new(handle, is_multiple);
    manager.did_open(&chooser);
  });
}

async fn dispatch_console(
  params: &Value,
  console_log: &Arc<RwLock<Vec<ConsoleMessage>>>,
  emitter: &crate::events::EventEmitter,
  page_backref: &crate::backend::PageBackref,
) {
  let Some(message) = params.get("message") else {
    return;
  };
  let Some(page) = page_backref.upgrade() else {
    return;
  };
  let level = message.get("level").and_then(Value::as_str).unwrap_or("log");
  let source = message.get("source").and_then(Value::as_str).unwrap_or("");
  // Per `wkPage.ts::_onConsoleMessage`: a JS-source error fires
  // `addPageError` instead of a console message. Map to
  // [`PageEvent::PageError`].
  if level == "error" && source == "javascript" {
    let raw = message.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    let (name, msg_body) = match raw.find(": ") {
      Some(idx) => (raw[..idx].to_string(), raw[idx + 2..].to_string()),
      None => (String::new(), raw.clone()),
    };
    let stack = build_stack(&raw, message.get("stackTrace"));
    let err = crate::web_error::ErrorDetails::new(name, msg_body, stack);
    emitter.emit(crate::events::PageEvent::PageError(crate::web_error::WebError::new(
      &page, err,
    )));
    return;
  }
  let ty = match level {
    "error" => "error",
    "warning" => "warning",
    "debug" => "debug",
    "info" => "info",
    _ => "log",
  };
  let location = ConsoleMessageLocation {
    url: message.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
    line_number: u32::try_from(message.get("line").and_then(Value::as_u64).unwrap_or(1)).unwrap_or(1),
    column_number: u32::try_from(message.get("column").and_then(Value::as_u64).unwrap_or(1)).unwrap_or(1),
  };
  let mut args = Vec::new();
  if let Some(parameters) = message.get("parameters").and_then(Value::as_array) {
    for arg in parameters {
      let backing = pw_remote_object_to_backing(arg);
      let is_node = arg.get("subtype").and_then(Value::as_str) == Some("node");
      args.push(crate::js_handle::JSHandle::from_backing(page.clone(), backing, is_node));
    }
  }
  // PW `WebKit`'s `message.text` only carries the first formatted
  // token — defer to [`ConsoleMessage`]'s arg-join fallback by passing
  // `None`, so `text()` reconstructs the full message from args.
  let explicit_text = if args.is_empty() {
    Some(message.get("text").and_then(Value::as_str).unwrap_or("").to_string())
  } else {
    None
  };
  let msg = ConsoleMessage::new(&page, ty, explicit_text, args, location, 0);
  console_log.write().await.push(msg.clone());
  emitter.emit(crate::events::PageEvent::Console(msg));
}

/// Build a JS-error stack string from PW `WebKit`'s `stackTrace` payload.
/// Mirrors `wkPage.ts`: first line is the raw text, then one
/// `    at <fn> (<url>:<line>:<col>)` line per `stackTrace.callFrames`.
fn build_stack(text: &str, stack: Option<&Value>) -> String {
  use std::fmt::Write as _;
  let mut out = text.to_string();
  let Some(frames) = stack.and_then(|s| s.get("callFrames")).and_then(Value::as_array) else {
    return String::new();
  };
  for frame in frames {
    let function_name = frame.get("functionName").and_then(Value::as_str).unwrap_or("unknown");
    let url = frame.get("url").and_then(Value::as_str).unwrap_or("");
    let line = frame.get("lineNumber").and_then(Value::as_u64).unwrap_or(0);
    let col = frame.get("columnNumber").and_then(Value::as_u64).unwrap_or(0);
    out.push('\n');
    let _ = write!(out, "    at {function_name} ({url}:{line}:{col})");
  }
  out
}

/// Build a live [`crate::dialog::Dialog`] handle from a
/// `Dialog.javascriptDialogOpening` payload and route it through
/// [`crate::dialog::DialogManager::did_open`]. The responder closure
/// invokes `Dialog.handleJavaScriptDialog` on the page-proxy session —
/// `accept` + `promptText`. When no handler claims the dialog, the
/// manager auto-closes (`accept` for `beforeunload`, dismiss otherwise),
/// matching the CDP backend's behaviour.
async fn dispatch_dialog(
  proxy: &Session,
  params: &Value,
  dialog_manager: &crate::dialog::DialogManager,
  dialog_log: &Arc<RwLock<Vec<DialogEvent>>>,
  page: std::sync::Weak<crate::page::Page>,
) {
  let dialog_type_str = params
    .get("type")
    .and_then(Value::as_str)
    .unwrap_or("alert")
    .to_string();
  let message = params.get("message").and_then(Value::as_str).unwrap_or("").to_string();
  let default_value = params
    .get("defaultPrompt")
    .and_then(Value::as_str)
    .unwrap_or("")
    .to_string();
  let dialog_type = crate::dialog::DialogType::parse(&dialog_type_str);

  let proxy_for_responder = proxy.clone();
  let responder: crate::dialog::DialogResponder = Arc::new(move |response| {
    let proxy = proxy_for_responder.clone();
    Box::pin(async move {
      let mut cmd_params = json!({
        "accept": matches!(response, crate::dialog::DialogResponse::Accept { .. }),
      });
      if let crate::dialog::DialogResponse::Accept {
        prompt_text: Some(text),
      } = response
      {
        cmd_params["promptText"] = Value::String(text);
      }
      proxy
        .send("Dialog.handleJavaScriptDialog", cmd_params)
        .await
        .map(|_| ())
        .map_err(|e| crate::error::FerriError::backend(format!("webkit dialog: {e}")))
    })
  });
  let dialog = crate::dialog::Dialog::new_with_manager(
    dialog_type,
    message.clone(),
    default_value,
    responder,
    Some(dialog_manager.clone()),
    page,
  );
  dialog_manager.did_open(dialog);
  dialog_log.write().await.push(DialogEvent {
    dialog_type: dialog_type_str,
    message,
    action: "dispatched".to_string(),
  });
}
