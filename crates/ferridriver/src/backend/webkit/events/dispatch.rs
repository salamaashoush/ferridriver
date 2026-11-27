//! `WebKit` frame, execution-context, console, and dialog event dispatch.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;

use super::TargetListenerCtx;
use super::network::pw_remote_object_to_backing;
use crate::backend::webkit::connection::Session;
use crate::console_message::{ConsoleMessage, ConsoleMessageLocation};
use crate::context::DialogEvent;

/// Build a [`ConsoleMessage`] from a `Console.messageAdded` payload
/// and dispatch it through both the per-context log and the page's
/// event emitter. Drops the event when no outer `Arc<Page>` is
/// reachable through `page_backref` — matches Playwright's
/// `createHandle(context, arg)` guard.
pub(super) type Routes = Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>;
pub(super) type FrameContexts = Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, i64>>>;
pub(super) type FrameCache = Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>;

pub(super) async fn handle_exec_context_created(
  params: &Value,
  frame_contexts: &FrameContexts,
  utility_contexts: &FrameContexts,
) {
  let Some(ctx) = params.get("context") else {
    return;
  };
  if ctx.get("type").and_then(Value::as_str) == Some("user")
    && ctx.get("name").and_then(Value::as_str) == Some(crate::backend::webkit::page::UTILITY_WORLD_NAME)
    && let (Some(frame_id), Some(id)) = (
      ctx.get("frameId").and_then(Value::as_str),
      ctx.get("id").and_then(Value::as_i64),
    )
  {
    utility_contexts.write().await.insert(frame_id.to_string(), id);
    return;
  }
  // Track only the frame's normal (main-world) context — mirrors the CDP
  // tracker's `isDefault` filter. WebKit announces the utility world
  // (`__playwright_utility_world__`, type "user") right after the normal
  // one for the same frameId; letting it overwrite the entry anchored
  // frame-scoped evaluates in the utility world, where page-visible
  // globals (e.g. the WebSocket mock's dispatch hook) don't exist.
  if ctx.get("type").and_then(Value::as_str) != Some("normal") {
    return;
  }
  let Some(frame_id) = ctx.get("frameId").and_then(Value::as_str) else {
    return;
  };
  let Some(id) = ctx.get("id").and_then(Value::as_i64) else {
    return;
  };
  frame_contexts.write().await.insert(frame_id.to_string(), id);
}

/// Keeps a page's frame cache in wire order, from the connection's
/// reader (see [`crate::backend::webkit::connection::RouteObserver`]), as the CDP
/// transport does. A reply that follows a frame event on the wire must
/// find the cache already moved: `page.url()` read the moment an evaluate
/// that ran `history.pushState` returns has to be the pushed URL, and the
/// listener that emits the page's events can lag that reply by however
/// many events it still has queued. A provisional target's frame events
/// wait for its commit, which first empties the cache for the new process.
#[derive(Clone)]
pub(crate) struct FrameTracker {
  cache: FrameCache,
  provisional: Arc<std::sync::Mutex<rustc_hash::FxHashMap<String, Vec<crate::backend::webkit::protocol::Envelope>>>>,
}

impl FrameTracker {
  pub(crate) fn new(cache: FrameCache) -> Self {
    Self {
      cache,
      provisional: Arc::default(),
    }
  }

  /// Track the frames of the committed target `target`.
  pub(crate) fn track_target(&self, target: &Session) {
    let cache = Arc::clone(&self.cache);
    target.observe(Arc::new(move |env| {
      apply_frame_event(&mut lock_cache(&cache), env);
    }));
  }

  /// Track the provisional targets `proxy` announces and their commits.
  pub(crate) fn track_proxy(&self, proxy: &Session) {
    let tracker = self.clone();
    let session = proxy.clone();
    proxy.observe(Arc::new(move |env| match env.method.as_deref() {
      Some("Target.targetCreated") => {
        let Some(info) = env.params.get("targetInfo") else {
          return;
        };
        let provisional = info.get("isProvisional").and_then(Value::as_bool).unwrap_or(false);
        let Some(target_id) = info.get("targetId").and_then(Value::as_str) else {
          return;
        };
        if !provisional || info.get("type").and_then(Value::as_str) != Some("page") {
          return;
        }
        tracker.provisional_target(&session, target_id);
      },
      Some("Target.didCommitProvisionalTarget") => {
        if let Some(target_id) = env.params.get("newTargetId").and_then(Value::as_str) {
          tracker.commit(target_id);
        }
      },
      _ => {},
    }));
  }

  fn provisional_target(&self, proxy: &Session, target_id: &str) {
    lock_provisional(&self.provisional).insert(target_id.to_string(), Vec::new());
    let target = proxy
      .connection_handle()
      .target_session(proxy.page_proxy_id().unwrap_or_default(), target_id);
    let tracker = self.clone();
    let target_id = target_id.to_string();
    target.observe(Arc::new(move |env| {
      if !is_frame_event(env) {
        return;
      }
      if let Some(held) = lock_provisional(&tracker.provisional).get_mut(&target_id) {
        held.push(env.clone());
        return;
      }
      apply_frame_event(&mut lock_cache(&tracker.cache), env);
    }));
  }

  /// The committed target lives in a fresh process: the frames of the old
  /// one are gone, and what the new one reported so far applies now.
  fn commit(&self, target_id: &str) {
    let held = lock_provisional(&self.provisional)
      .remove(target_id)
      .unwrap_or_default();
    let mut cache = lock_cache(&self.cache);
    cache.new_process();
    for env in &held {
      apply_frame_event(&mut cache, env);
    }
  }
}

pub(super) fn lock_cache(cache: &FrameCache) -> std::sync::MutexGuard<'_, crate::frame_cache::FrameCache> {
  cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Held = rustc_hash::FxHashMap<String, Vec<crate::backend::webkit::protocol::Envelope>>;

fn lock_provisional(provisional: &std::sync::Mutex<Held>) -> std::sync::MutexGuard<'_, Held> {
  provisional.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn is_frame_event(env: &crate::backend::webkit::protocol::Envelope) -> bool {
  matches!(
    env.method.as_deref(),
    Some("Page.frameAttached" | "Page.frameNavigated" | "Page.navigatedWithinDocument" | "Page.frameDetached")
  )
}

pub(super) fn apply_frame_event(
  cache: &mut crate::frame_cache::FrameCache,
  env: &crate::backend::webkit::protocol::Envelope,
) {
  let params = &env.params;
  let frame_id = || params.get("frameId").and_then(Value::as_str);
  match env.method.as_deref() {
    Some("Page.frameAttached") => {
      if let Some(frame_id) = frame_id() {
        cache.attach(crate::backend::FrameInfo {
          frame_id: frame_id.to_string(),
          parent_frame_id: params.get("parentFrameId").and_then(Value::as_str).map(str::to_string),
          name: String::new(),
          url: String::new(),
        });
      }
    },
    Some("Page.frameNavigated") => {
      if let Some(info) = navigated_frame(params) {
        cache.navigated(info);
      }
    },
    Some("Page.navigatedWithinDocument") => {
      if let Some(frame_id) = frame_id() {
        cache.navigated_within(frame_id, params.get("url").and_then(Value::as_str).unwrap_or(""));
      }
    },
    Some("Page.frameDetached") => {
      if let Some(frame_id) = frame_id() {
        cache.detach(frame_id);
      }
    },
    _ => {},
  }
}

pub(super) fn handle_frame_attached(params: &Value, emitter: &crate::events::EventEmitter) {
  let Some(frame_id) = params.get("frameId").and_then(Value::as_str) else {
    return;
  };
  let parent_id = params
    .get("parentFrameId")
    .and_then(Value::as_str)
    .map(std::string::ToString::to_string);
  emitter.emit(crate::events::PageEvent::FrameAttached(crate::backend::FrameInfo {
    frame_id: frame_id.to_string(),
    parent_frame_id: parent_id,
    name: String::new(),
    url: String::new(),
  }));
}

pub(super) fn handle_frame_navigated(params: &Value, emitter: &crate::events::EventEmitter) {
  if let Some(info) = navigated_frame(params) {
    emitter.emit(crate::events::PageEvent::FrameNavigated(info));
  }
}

/// The frame a `Page.frameNavigated` describes.
fn navigated_frame(params: &Value) -> Option<crate::backend::FrameInfo> {
  let frame = params.get("frame")?;
  Some(crate::backend::FrameInfo {
    frame_id: frame.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
    parent_frame_id: frame
      .get("parentId")
      .and_then(Value::as_str)
      .map(std::string::ToString::to_string),
    name: frame.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
    url: frame.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
  })
}

/// Same-document navigation (`history.pushState` / `replaceState` /
/// fragment): `WebKit` sends `{ frameId, url }`. URL-only cache update —
/// the document (and any child frames) persists. Mirrors Playwright's
/// `wkPage.ts::_onFrameNavigatedWithinDocument`.
pub(super) fn handle_frame_navigated_within(params: &Value, emitter: &crate::events::EventEmitter) {
  let Some(frame_id) = params.get("frameId").and_then(Value::as_str) else {
    return;
  };
  let url = params.get("url").and_then(Value::as_str).unwrap_or("");
  emitter.emit(crate::events::PageEvent::FrameNavigatedWithinDocument(
    crate::backend::FrameInfo {
      frame_id: frame_id.to_string(),
      parent_frame_id: None,
      name: String::new(),
      url: url.to_string(),
    },
  ));
}

pub(super) async fn handle_frame_detached(params: &Value, ctx: &TargetListenerCtx) {
  let Some(frame_id) = params.get("frameId").and_then(Value::as_str) else {
    return;
  };
  ctx.frame_contexts.write().await.remove(frame_id);
  ctx.utility_contexts.write().await.remove(frame_id);
  ctx.emitter_frame.emit(crate::events::PageEvent::FrameDetached {
    frame_id: frame_id.to_string(),
  });
}

pub(super) async fn dispatch_console(
  params: &Value,
  console_log: &Arc<RwLock<Vec<ConsoleMessage>>>,
  emitter: &crate::events::EventEmitter,
  page_backref: &crate::backend::PageBackref,
) {
  let Some(message) = params.get("message") else {
    return;
  };
  let level = message.get("level").and_then(Value::as_str).unwrap_or("log");
  let source = message.get("source").and_then(Value::as_str).unwrap_or("");
  // Per `wkPage.ts::_onConsoleMessage`: a JS-source error fires
  // `addPageError` instead of a console message. Map to
  // [`PageEvent::PageError`].
  if level == "error" && source == "javascript" {
    let raw = message.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    let (name, msg_body) = match raw.split_once(": ") {
      Some((name, body)) => (name.to_string(), body.to_string()),
      None => (String::new(), raw.clone()),
    };
    let stack = build_stack(&raw, message.get("stackTrace"));
    let err = crate::web_error::ErrorDetails::new(name, msg_body, stack);
    // `wkPage.ts:549`: location is the message-level `url`/`line`/`column`
    // (1-based, converted to 0-based via `(v || 1) - 1`), not the stack.
    let location = crate::console_message::ConsoleMessageLocation {
      url: message.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
      line_number: u32::try_from(message.get("line").and_then(Value::as_u64).unwrap_or(1).max(1) - 1)
        .unwrap_or(u32::MAX),
      column_number: u32::try_from(message.get("column").and_then(Value::as_u64).unwrap_or(1).max(1) - 1)
        .unwrap_or(u32::MAX),
    };
    crate::backend::shared_page::dispatch_page_error_event(page_backref, emitter, err, location);
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
  let mut arg_backings = Vec::new();
  if let Some(parameters) = message.get("parameters").and_then(Value::as_array) {
    for arg in parameters {
      let backing = pw_remote_object_to_backing(arg);
      let is_node = arg.get("subtype").and_then(Value::as_str) == Some("node");
      arg_backings.push((backing, is_node));
    }
  }
  // PW `WebKit`'s `message.text` only carries the first formatted
  // token — defer to [`ConsoleMessage`]'s arg-join fallback by passing
  // `None`, so `text()` reconstructs the full message from args.
  let explicit_text = if arg_backings.is_empty() {
    Some(message.get("text").and_then(Value::as_str).unwrap_or("").to_string())
  } else {
    None
  };
  crate::backend::shared_page::dispatch_console_event(
    page_backref,
    console_log,
    emitter,
    crate::backend::shared_page::ConsoleEventInput {
      kind: ty.to_string(),
      explicit_text,
      arg_backings,
      location,
      timestamp: 0,
    },
  )
  .await;
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
pub(super) async fn dispatch_dialog(
  proxy: &Session,
  params: &Value,
  dialog_manager: &crate::dialog::DialogManager,
  dialog_log: &Arc<RwLock<Vec<DialogEvent>>>,
  page: std::sync::Weak<crate::page::Page>,
) {
  let Ok(payload) =
    <ferridriver_webkit_proto::page::JavascriptDialogOpeningEvent as serde::Deserialize>::deserialize(params)
  else {
    return;
  };
  let dialog_type_str = if payload.dialog_type.is_empty() {
    "alert".to_string()
  } else {
    payload.dialog_type
  };
  let message = payload.message;
  let default_value = payload.default_prompt;
  let dialog_type = crate::dialog::DialogType::parse(&dialog_type_str);

  let proxy_for_responder = proxy.clone();
  let responder: crate::dialog::DialogResponder = Arc::new(move |response| {
    let proxy = proxy_for_responder.clone();
    Box::pin(async move {
      let (accept, prompt_text) = match response {
        crate::dialog::DialogResponse::Accept { prompt_text } => (true, prompt_text),
        crate::dialog::DialogResponse::Dismiss => (false, None),
      };
      proxy
        .send_typed(&ferridriver_webkit_proto::dialog::HandleJavaScriptDialog {
          accept,
          prompt_text: prompt_text.as_deref(),
        })
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
  crate::state::push_capped(
    &mut *dialog_log.write().await,
    DialogEvent {
      dialog_type: dialog_type_str,
      message,
      action: "dispatched".to_string(),
    },
    crate::state::DIALOG_LOG_CAP,
  );
}
