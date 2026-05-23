//! PW `WebKit` event translation: protocol events → ferridriver
//! [`ConsoleMessage`] / [`DialogEvent`] / accessibility / cookie types.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio::sync::broadcast::error::RecvError;

use super::connection::Session;
use super::page::PwWebKitPage;
use crate::backend::{AxNodeData, AxProperty, CookieData, SameSite};
use crate::console_message::{ConsoleMessage, ConsoleMessageLocation};
use crate::context::DialogEvent;
use crate::error::{FerriError, Result};
use crate::network::{
  BodyFn, Headers, RemoteAddr, Request as NetworkRequest, RequestInit, Response, ResponseInit, SecurityDetails,
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
    for (name, key) in [("disabled", "disabled"), ("readonly", "readonly"), ("required", "required")] {
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
  let same_site = c
    .get("sameSite")
    .and_then(Value::as_str)
    .and_then(|s| match s {
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
  if let Some(ss) = cookie.same_site {
    obj["sameSite"] = json!(ss.as_str());
  }
  // PW WebKit `Page.setCookie` derives the URL from domain+path+secure
  // when no explicit url is given.
  let scheme = if cookie.secure { "https" } else { "http" };
  obj["url"] = json!(format!("{scheme}://{}", obj["domain"].as_str().unwrap_or("")));
  target
    .send("Page.setCookie", json!({ "cookie": obj }))
    .await
    .map_err(|e| FerriError::backend(format!("pw-webkit set_cookie: {e}")))?;
  Ok(())
}

/// Spawn the per-page listener loop: translates `Console.messageAdded`
/// into [`ConsoleMessage`]s and auto-handles `Dialog.*` so a dialog
/// never wedges a test. Network capture is handled separately.
pub fn attach_listeners(
  page: &PwWebKitPage,
  console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
  network_log: Arc<RwLock<Vec<NetworkRequest>>>,
  dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
) {
  let mut target_rx = page.target_session().events();
  let mut proxy_rx = page.proxy_session().events();
  let target = page.target_session().clone();
  let proxy = page.proxy_session().clone();
  let dialog_manager = page.dialog_manager.clone();
  let file_chooser_manager = page.file_chooser_manager.clone();
  let page_backref = page.page_backref.clone();
  let emitter = page.events.clone();
  let requests = page.requests.clone();
  let nav_slot = page.nav_request_slot.clone();
  let _ = dialog_manager.register_emitter_bridge(emitter.clone());

  tokio::spawn(async move {
    loop {
      tokio::select! {
        ev = target_rx.recv() => match ev {
          Ok(env) => {
            match env.method.as_deref() {
              Some("Console.messageAdded") => {
                dispatch_console(&env.params, &console_log, &emitter, &page_backref).await;
              },
              Some("Network.requestWillBeSent") => {
                handle_request_will_be_sent(&env.params, &requests, &nav_slot, &network_log, &emitter).await;
              },
              Some("Network.responseReceived") => {
                handle_response_received(&env.params, &requests, &target, &emitter).await;
              },
              Some("Network.loadingFinished") => {
                handle_loading_finished(&env.params, &requests, &emitter);
              },
              Some("Network.loadingFailed") => {
                handle_loading_failed(&env.params, &requests, &emitter);
              },
              Some("Page.fileChooserOpened") => {
                dispatch_file_chooser(&env.params, &target, &page_backref, &file_chooser_manager);
              },
              _ => {},
            }
          },
          Err(RecvError::Lagged(_)) => {},
          Err(RecvError::Closed) => break,
        },
        ev = proxy_rx.recv() => match ev {
          Ok(env) => {
            if env.method.as_deref() == Some("Dialog.javascriptDialogOpening") {
              dispatch_dialog(&proxy, &env.params, &dialog_manager, &dialog_log).await;
            }
          },
          Err(RecvError::Lagged(_)) => {},
          Err(RecvError::Closed) => break,
        },
      }
    }
  });
}

type Requests = Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, NetworkRequest>>>;

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
    return JSHandleBacking::Remote(HandleRemote::PwWebKit(Arc::from(obj_id)));
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
    let element = super::element::PwWebKitElement::new(target, object_id);
    let any_element = crate::backend::AnyElement::PwWebKit(element);
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
    emitter.emit(crate::events::PageEvent::PageError(crate::web_error::WebError::new(&page, err)));
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
) {
  let dialog_type_str = params.get("type").and_then(Value::as_str).unwrap_or("alert").to_string();
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
        .map_err(|e| crate::error::FerriError::backend(format!("pw-webkit dialog: {e}")))
    })
  });
  let dialog = crate::dialog::Dialog::new_with_manager(
    dialog_type,
    message.clone(),
    default_value,
    responder,
    Some(dialog_manager.clone()),
  );
  dialog_manager.did_open(dialog);
  dialog_log.write().await.push(DialogEvent {
    dialog_type: dialog_type_str,
    message,
    action: "dispatched".to_string(),
  });
}
