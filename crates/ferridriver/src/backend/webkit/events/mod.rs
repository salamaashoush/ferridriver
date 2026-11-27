//! PW `WebKit` event translation: protocol events → ferridriver
//! `ConsoleMessage` / `DialogEvent` / accessibility / cookie types.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;

use super::page::WebKitPage;
use crate::backend::{AxNodeData, AxProperty, CookieData, SameSite};
use crate::console_message::ConsoleMessage;
use crate::network::Request as NetworkRequest;

/// Parse the JSON array produced by `window.__fd.accessibilityTree(depth)`
/// into [`AxNodeData`]. Shared shape with the `BiDi` backend.
#[must_use]
pub fn parse_ax_nodes(arr: &[Value]) -> Vec<AxNodeData> {
  let mut nodes = Vec::with_capacity(arr.len());
  for item in arr {
    let mut properties = Vec::new();
    let mut push_str = |name: &str, key: &str| {
      if let Some(v) = item.get(key).and_then(Value::as_str)
        && !v.is_empty()
      {
        properties.push(AxProperty {
          name: name.to_string(),
          value: Some(Value::String(v.to_string())),
        });
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

/// `-1` is the "session cookie" sentinel on the wire — an exact literal,
/// never a computed float, so the comparison is by fixed margin only to
/// satisfy `float_cmp`.
fn is_session_sentinel(expires: f64) -> bool {
  (expires + 1.0).abs() < f64::EPSILON
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
    // WebKit reports `expires` in MILLISECONDS; the user-facing value is
    // unix seconds. Playwright: `expires === -1 ? -1 : expires / 1000`
    // (wkBrowser.ts::cookies).
    expires: c
      .get("expires")
      .and_then(Value::as_f64)
      .map(|e| if is_session_sentinel(e) { e } else { e / 1000.0 }),
    same_site,
    url: None,
  }
}

/// Build a `Playwright.setCookies` `SetCookieParam` (browser-context-wide, no
/// `url` field — domain/path/secure carry the scope). Mirrors Playwright's
/// `wkBrowser.ts::addCookies` mapping. Cookies are set through the root
/// browser session so they populate the whole context's store, not just the
/// current page's reachable origin.
#[must_use]
pub fn webkit_set_cookie_param(cookie: &CookieData) -> Value {
  let mut obj = json!({
    "name": cookie.name,
    "value": cookie.value,
    "domain": cookie.domain,
    "path": if cookie.path.is_empty() { "/".to_string() } else { cookie.path.clone() },
    "secure": cookie.secure,
    "httpOnly": cookie.http_only,
    "session": cookie.expires.is_none_or(is_session_sentinel),
  });
  // WebKit's `SetCookieParam.expires` is MILLISECONDS; the user-facing
  // value is unix seconds. Playwright: `expires && expires !== -1 ?
  // expires * 1000 : expires` (wkBrowser.ts::addCookies).
  if let Some(exp) = cookie.expires {
    obj["expires"] = if is_session_sentinel(exp) {
      json!(exp)
    } else {
      json!(exp * 1000.0)
    };
  }
  // Omit `sameSite` when unset (Playwright passes it through as
  // undefined). Forcing "None" broke cookie writes on Linux WebKit:
  // libsoup rejects `SameSite=None` without `Secure`, while macOS
  // CFNetwork accepts it — the Linux CI run stored no cookies at all.
  if let Some(ss) = cookie.same_site {
    obj["sameSite"] = json!(ss.as_str());
  }
  obj
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
/// Drive one page's target + page-proxy event loops.
///
/// The receivers are passed IN rather than subscribed here: `attach`
/// spends half a dozen awaited protocol calls before it can build the
/// page, and a popup's own navigation creates its provisional target
/// inside that window. Subscribing at the end lost that
/// `Target.targetCreated`, so the paused provisional was never resumed
/// and the popup sat on `about:blank` forever. The channels are
/// unbounded, so subscribing first and draining later replays
/// everything in wire order.
pub fn attach_listeners(
  page: &WebKitPage,
  mut target_rx: tokio::sync::mpsc::UnboundedReceiver<super::protocol::Envelope>,
  mut proxy_rx: tokio::sync::mpsc::UnboundedReceiver<super::protocol::Envelope>,
) {
  let proxy = page.proxy_session().clone();
  let dialog_manager = page.dialog_manager.clone();
  let file_chooser_manager = page.file_chooser_manager.clone();
  let page_backref = page.page_backref.clone();
  let emitter = page.events.clone();
  let requests = Arc::clone(&page.requests);
  let nav_slot = page.nav_request_slot.clone();
  let routes = Arc::clone(&page.routes);
  let frame_contexts = Arc::clone(&page.frame_contexts);
  let utility_contexts = Arc::clone(&page.utility_contexts);
  let websockets = Arc::clone(&page.websockets);
  let emitter_frame = emitter.clone();
  let _ = dialog_manager.register_emitter_bridge(emitter.clone());

  let (binding_tx, mut binding_rx) = tokio::sync::mpsc::unbounded_channel::<futures::future::BoxFuture<'static, ()>>();
  let mut binding_worker = tokio::task::JoinSet::new();
  binding_worker.spawn(async move {
    while let Some(call) = binding_rx.recv().await {
      call.await;
    }
  });
  let ctx = TargetListenerCtx {
    binding_tx,
    target_swap: page.target_swap(),
    emitter,
    emitter_frame,
    page_backref,
    file_chooser_manager,
    requests,
    nav_slot,
    routes,
    frame_contexts,
    utility_contexts,
    websockets,
    workers: Arc::clone(&page.workers),
    console_log: Arc::clone(&page.console_log),
    network_log: Arc::clone(&page.network_log),
    lifecycle: Arc::clone(&page.lifecycle),
    main_frame_id_cache: Arc::clone(&page.main_frame_id_cache),
    engine_injected: page.engine_injected_flag(),
    global_object_id: page.global_object_id_slot(),
    exposed_fns: Arc::clone(&page.exposed_fns),
    page_proxy_id: Arc::from(page.page_proxy_id()),
  };
  let dialog_log = Arc::clone(&page.dialog_log);
  // Provisional-target slot. Populated on `Target.targetCreated` with
  // `isProvisional: true`; consumed on `Target.didCommitProvisionalTarget`
  // to swap the page's live target session.
  let provisional: ProvisionalSlot = Arc::new(tokio::sync::Mutex::new(None));
  let page = page.clone();
  tokio::spawn(async move {
    let _binding_worker = binding_worker;
    loop {
      tokio::select! {
        ev = target_rx.recv() => match ev {
          Some(env) => dispatch_target_event(&ctx, env).await,
          None => break,
        },
        ev = proxy_rx.recv() => match ev {
          Some(env) => match env.method.as_deref() {
            Some("Dialog.javascriptDialogOpening") => {
              let log = arc_swap::Guard::into_inner(dialog_log.load());
              dispatch_dialog(&proxy, &env.params, &dialog_manager, &log, page.page_backref.weak()).await;
            },
            Some("Target.targetCreated") => {
              handle_provisional_target_created(&env.params, &page, Arc::clone(&provisional)).await;
            },
            // A destroyed target answers nothing ever again. Without
            // this, a call already on the wire when the target went away
            // waits for a response that cannot arrive — and a caller
            // whose own deadline is only checked between polls (
            // `wait_for_function`, the locator retry loop) never gets
            // back to check it. A cross-process navigation destroys the
            // old target the moment its provisional replacement commits,
            // so this is the ordinary path, not a crash path.
            Some("Target.targetDestroyed") => {
              if let Some(gone) = env.params.get("targetId").and_then(Value::as_str) {
                page.proxy_session().connection_handle().close_route(None, Some(gone));
              }
              if live_target_crashed(&env.params, &page.target_id()) {
                page.events.emit(crate::events::PageEvent::Crash);
              }
            },
            Some("Target.didCommitProvisionalTarget") => {
              if let Some(new_rx) =
                handle_committed_provisional_target(&env.params, &page, Arc::clone(&provisional)).await
              {
                target_rx = new_rx;
              }
            },
            _ => {},
          },
          None => break,
        },
      }
    }
  });
}

/// Whether a `Target.targetDestroyed` reports the page's own target
/// crashing (`wkPage._onTargetDestroyed`); a provisional or replaced
/// target going away is ordinary navigation.
fn live_target_crashed(params: &Value, live_target: &str) -> bool {
  params.get("crashed").and_then(Value::as_bool) == Some(true)
    && params.get("targetId").and_then(Value::as_str) == Some(live_target)
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
  // The workers belonged to the document the old target held.
  super::worker::terminated(None, &page.workers);
  // Pipeline every domain enable and state replay command in exact FIFO
  // wire order before resuming the provisional target.
  let mut setup_cmds: Vec<(&'static str, Value)> = vec![
    ("Page.enable", json!({})),
    ("Runtime.enable", json!({})),
    ("Network.enable", json!({})),
    ("Console.enable", json!({})),
    ("Worker.enable", json!({})),
  ];
  let extra_headers = page
    .extra_http_headers
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  if let Some(headers) = extra_headers {
    setup_cmds.push(("Network.setExtraHTTPHeaders", json!({ "headers": headers })));
  }
  let emulated = page
    .emulated_media
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone();
  if let Some(opts) = emulated {
    setup_cmds.extend(super::page::WebKitPage::emulate_media_commands(&opts));
  }
  if page.interception_enabled() {
    setup_cmds.push(("Network.setInterceptionEnabled", json!({ "enabled": true })));
    setup_cmds.push(("Network.setResourceCachingDisabled", json!({ "disabled": true })));
    setup_cmds.push((
      "Network.addInterception",
      json!({ "url": ".*", "stage": "request", "isRegex": true }),
    ));
  }
  setup_cmds.push((
    "Page.createUserWorld",
    json!({ "name": super::page::UTILITY_WORLD_NAME }),
  ));
  let bootstrap = page.bootstrap_source().await;
  setup_cmds.push(("Page.setBootstrapScript", json!({ "source": bootstrap })));
  if page.binding_channel_active() {
    setup_cmds.push(("Runtime.addBinding", json!({ "name": "__fd_binding__" })));
  }
  futures::future::join_all(setup_cmds.into_iter().map(|(method, params)| {
    let session = new_target.clone();
    async move {
      let _ = session.send(method, params).await;
    }
  }))
  .await;
  // Drain the provisional session's events NOW instead of waiting for
  // commit. The provisional target carries the navigation request; when
  // that request is intercepted, `Network.requestIntercepted` arrives on
  // this session BEFORE the commit — and the commit cannot happen until
  // the paused request is resolved. Subscribing only at commit therefore
  // deadlocks every routed cross-document navigation. The drain handles
  // interception events inline (for this target's whole life — the main
  // listener never sees them, so there is no double resolution) and
  // forwards everything else into the channel the commit handler swaps
  // in as the new `target_rx`. Mirrors `WKProvisionalPage`, which
  // installs session listeners the moment the provisional page exists.
  let (fwd_tx, fwd_rx) = tokio::sync::mpsc::unbounded_channel();
  let mut raw_rx = new_target.events();
  let drain_session = new_target.clone();
  let drain_routes = Arc::clone(&page.routes);
  tokio::spawn(async move {
    while let Some(env) = raw_rx.recv().await {
      if env.method.as_deref() == Some("Network.requestIntercepted") {
        handle_request_intercepted(&env.params, &drain_session, &drain_routes);
        continue;
      }
      // Receiver dropped = this provisional was superseded or its
      // committed target got swapped out again — stop draining.
      if fwd_tx.send(env).is_err() {
        break;
      }
    }
  });
  // Stash before resuming — a fast commit could fire before `await`
  // releases here, and the swap reader needs to find the session.
  {
    let mut slot = provisional.lock().await;
    *slot = Some((new_target, Arc::<str>::from(target_id.clone()), fwd_rx));
  }
  if info.get("isPaused").and_then(Value::as_bool).unwrap_or(false) {
    let _ = proxy
      .send_typed(&ferridriver_webkit_proto::target::Resume { target_id: &target_id })
      .await;
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
) -> Option<tokio::sync::mpsc::UnboundedReceiver<super::protocol::Envelope>> {
  let ev =
    serde_json::from_value::<ferridriver_webkit_proto::target::DidCommitProvisionalTargetEvent>(params.clone()).ok()?;
  let new_target_id = ev.new_target_id;
  let (new_session, stashed_id, fwd_rx) = provisional.lock().await.take()?;
  if &*stashed_id != new_target_id.as_str() {
    // Defensive: if WebKit committed a target other than the one we
    // stashed, drop the stash and let the next attach cycle recover.
    return None;
  }
  // `fwd_rx` is the forwarding end of the provisional drain spawned at
  // target creation — it already holds every event the new session
  // emitted before this commit (minus interception events, which the
  // drain resolves itself).
  page.swap_target_session(new_session, stashed_id);
  Some(fwd_rx)
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
  binding_tx: tokio::sync::mpsc::UnboundedSender<futures::future::BoxFuture<'static, ()>>,
  target_swap: Arc<arc_swap::ArcSwap<super::connection::Session>>,
  emitter: crate::events::EventEmitter,
  emitter_frame: crate::events::EventEmitter,
  page_backref: crate::backend::PageBackref,
  file_chooser_manager: crate::file_chooser::FileChooserManager,
  requests: Requests,
  nav_slot: crate::network::NavRequestSlot,
  routes: Routes,
  frame_contexts: FrameContexts,
  utility_contexts: FrameContexts,
  websockets: WebSockets,
  workers: super::worker::Workers,
  /// Sinks for captured events. Swapped in by
  /// [`super::page::WebKitPage::attach_listeners`]; the listener
  /// reads the current pointer on each event so post-attach calls land
  /// in the caller's logs.
  console_log: Arc<arc_swap::ArcSwap<RwLock<Vec<ConsoleMessage>>>>,
  network_log: Arc<arc_swap::ArcSwap<RwLock<Vec<NetworkRequest>>>>,
  lifecycle: Arc<super::page::LifecycleSignals>,
  main_frame_id_cache: Arc<std::sync::Mutex<Option<String>>>,
  /// Realm caches invalidated on main-frame cross-document commits.
  /// `goto`/`reload` reset them at call time, but browser-initiated
  /// navigations (a popup's first document, page-side `location =`)
  /// only surface here — without this reset the cached global anchor
  /// points into the dead document and every evaluate fails with
  /// "Missing injected script for given objectId".
  engine_injected: Arc<super::page::EngineLatch>,
  global_object_id: Arc<std::sync::Mutex<Option<String>>>,
  /// Exposed-binding registry, shared with the page. `Runtime.bindingCalled`
  /// is dispatched here (not from a separate task pinned to one target
  /// session) so exposed functions keep working after a cross-process
  /// navigation swaps the live target — the main listener follows the swap.
  exposed_fns: Arc<tokio::sync::RwLock<rustc_hash::FxHashMap<String, crate::backend::PageBinding>>>,
  /// Owning page's proxy id — stamped on every captured `Request` as
  /// its `page_guid` for context-level page attribution.
  page_proxy_id: Arc<str>,
}

impl TargetListenerCtx {
  fn target(&self) -> super::connection::Session {
    super::connection::Session::clone(&self.target_swap.load())
  }
}

/// Dispatch a `Runtime.bindingCalled` for `__fd_binding__`: look up the
/// exposed function by name, invoke it, and resolve/reject the page-side
/// `__fd_bc` promise via a `Runtime.evaluate` on the live (swap-following)
/// target session. Mirrors the controller protocol set up in
/// `WebKitPage::ensure_binding_channel`.
async fn handle_binding_called(ctx: &TargetListenerCtx, params: &Value) {
  if params.get("name").and_then(Value::as_str) != Some("__fd_binding__") {
    return;
  }
  let payload_str = params.get("argument").and_then(Value::as_str).unwrap_or("{}");
  let payload: Value = serde_json::from_str(payload_str).unwrap_or_default();
  let fn_name = payload.get("name").and_then(Value::as_str).unwrap_or("").to_string();
  let seq = payload.get("seq").and_then(Value::as_u64).unwrap_or(0);
  let args: Vec<Value> = payload
    .get("args")
    .and_then(Value::as_array)
    .cloned()
    .unwrap_or_default();

  // WebKit's `bindingCalled` carries the calling execution context id;
  // reverse it through the frame-context map so iframe callers surface
  // their own frame in the `BindingSource` (unmapped contexts fall back
  // to the main frame).
  let ctx_id = params.get("contextId").and_then(Value::as_i64);
  let main_frame = ctx
    .main_frame_id_cache
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
    .unwrap_or_default();
  let source_frame = match ctx_id {
    Some(id) => ctx
      .frame_contexts
      .read()
      .await
      .iter()
      .find_map(|(fid, &cid)| (cid == id).then(|| fid.clone()))
      .unwrap_or_else(|| main_frame.clone()),
    None => main_frame.clone(),
  };
  let source = crate::events::BindingSource {
    context: String::new(),
    page: main_frame,
    frame: source_frame,
    live: None,
  };

  let maybe_fn = ctx.exposed_fns.read().await.get(&fn_name).cloned();
  let target_swap = Arc::clone(&ctx.target_swap);
  let _ = ctx.binding_tx.send(Box::pin(async move {
    let outcome = match maybe_fn {
      Some(callback) => callback(source, args).await,
      None => Err(format!("Function not found: {fn_name}")),
    };
    let deliver_js = crate::backend::binding_settle_js(seq, &outcome);
    // Resolve in the calling context — each frame has its own `__fd_bc`
    // controller, so a default-context evaluate would strand an iframe
    // caller's promise.
    let mut eval_params = json!({ "expression": deliver_js });
    if let Some(id) = ctx_id {
      eval_params["contextId"] = json!(id);
    }
    let _ = target_swap
      .load()
      .send(super::protocol::RUNTIME_EVALUATE, eval_params)
      .await;
  }));
}

/// Main-document commit bookkeeping for `Page.frameNavigated`. Only
/// main-frame commits feed the lifecycle latch — child frame commits
/// would falsely mark navigation as complete on the page level. Also
/// re-seeds the main-frame-id cache (a cross-process target swap
/// cleared it) and drops the realm caches: `goto`/`reload` reset those
/// at call time, but browser-initiated navigations (a popup's first
/// document, page-side `location =`) only surface here — without the
/// reset the cached global anchor points into the dead document and
/// every evaluate fails with "Missing injected script for given
/// objectId".
fn handle_main_frame_commit(ctx: &TargetListenerCtx, params: &Value) {
  if params.get("frame").and_then(|f| f.get("parentId")).is_some() {
    return;
  }
  let loader_id = params
    .pointer("/frame/loaderId")
    .and_then(Value::as_str)
    .map(str::to_string);
  ctx.lifecycle.commit_document(loader_id.clone());
  if let Some(loader_id) = loader_id {
    ctx.nav_slot.commit_loader(loader_id);
  }
  ctx.engine_injected.document_replaced();
  *ctx
    .global_object_id
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  if let Some(new_main_id) = params.get("frame").and_then(|f| f.get("id")).and_then(Value::as_str) {
    let mut slot = ctx
      .main_frame_id_cache
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = Some(new_main_id.to_string());
  }
}

/// A failed request fails the pending navigation when it carries it.
fn fail_pending_document(ctx: &TargetListenerCtx, params: &Value) {
  if let Some(request_id) = params.get("requestId").and_then(Value::as_str) {
    let error_text = params
      .get("errorText")
      .and_then(Value::as_str)
      .unwrap_or("navigation failed");
    ctx.lifecycle.request_failed(request_id, error_text);
  }
}

/// A main-frame document request now carries the pending navigation.
fn bind_document_request(ctx: &TargetListenerCtx, params: &Value) {
  if params.get("type").and_then(Value::as_str) == Some("Document")
    && is_main_frame_event(ctx, params)
    && let Some(request_id) = params.get("requestId").and_then(Value::as_str)
  {
    ctx.lifecycle.bind_pending_request(request_id.to_string());
  }
}

/// Whether an event's `frameId` names the main frame. Unknown until the
/// first commit, and again after a cross-process swap, when every frame
/// is the main one.
fn is_main_frame_event(ctx: &TargetListenerCtx, params: &Value) -> bool {
  let main = ctx
    .main_frame_id_cache
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  match (main.as_deref(), params.get("frameId").and_then(Value::as_str)) {
    (Some(main), Some(frame)) => main == frame,
    _ => true,
  }
}

async fn dispatch_target_event(ctx: &TargetListenerCtx, env: super::protocol::Envelope) {
  match env.method.as_deref() {
    Some("Runtime.bindingCalled") => {
      handle_binding_called(ctx, &env.params).await;
    },
    Some("Console.messageAdded") => {
      let log = arc_swap::Guard::into_inner(ctx.console_log.load());
      dispatch_console(&env.params, &log, &ctx.emitter, &ctx.page_backref).await;
    },
    Some("Network.requestWillBeSent") => {
      bind_document_request(ctx, &env.params);
      let log = arc_swap::Guard::into_inner(ctx.network_log.load());
      handle_request_will_be_sent(
        &env.params,
        &ctx.requests,
        &ctx.nav_slot,
        &log,
        &ctx.emitter,
        &ctx.page_proxy_id,
      )
      .await;
    },
    Some("Network.responseReceived") => {
      let target = ctx.target();
      handle_response_received(&env.params, &ctx.requests, &target, &ctx.emitter).await;
    },
    Some("Network.loadingFinished") => {
      handle_loading_finished(&env.params, &ctx.requests, &ctx.emitter).await;
    },
    Some("Network.loadingFailed") => {
      fail_pending_document(ctx, &env.params);
      handle_loading_failed(&env.params, &ctx.requests, &ctx.emitter).await;
    },
    Some("Page.willCheckNavigationPolicy") => {
      if is_main_frame_event(ctx, &env.params) {
        ctx.lifecycle.unbind_pending_request();
      }
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
      if let Some(context) = env.params.pointer("/context/id").and_then(Value::as_i64) {
        ctx.engine_injected.context_created(context);
      }
      handle_exec_context_created(&env.params, &ctx.frame_contexts, &ctx.utility_contexts).await;
    },
    Some("Page.frameAttached") => {
      handle_frame_attached(&env.params, &ctx.emitter_frame);
    },
    Some("Page.frameNavigated") => {
      handle_main_frame_commit(ctx, &env.params);
      handle_frame_navigated(&env.params, &ctx.emitter_frame);
    },
    Some("Page.frameDetached") => {
      handle_frame_detached(&env.params, ctx).await;
    },
    Some("Page.navigatedWithinDocument") => {
      let frame_id = env.params.get("frameId").and_then(Value::as_str);
      let main_frame = ctx
        .main_frame_id_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
      handle_frame_navigated_within(&env.params, &ctx.emitter_frame);
      if main_frame.is_none() || main_frame.as_deref() == frame_id {
        ctx.lifecycle.mark_same_document();
      }
    },
    Some("Worker.workerCreated") => super::worker::created(&env.params, &ctx.target(), &ctx.workers, &ctx.emitter),
    Some("Worker.dispatchMessageFromWorker") => super::worker::dispatched(&env.params, &ctx.workers),
    Some("Worker.workerTerminated") => {
      super::worker::terminated(env.params.get("workerId").and_then(Value::as_str), &ctx.workers);
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
/// Stash for an in-flight provisional target: the opened session, its
/// target id, and the forwarding receiver of the drain task spawned at
/// creation (becomes the listener's `target_rx` on commit).
type ProvisionalSlot = Arc<
  tokio::sync::Mutex<
    Option<(
      super::connection::Session,
      Arc<str>,
      tokio::sync::mpsc::UnboundedReceiver<super::protocol::Envelope>,
    )>,
  >,
>;

mod dispatch;
mod interception;
mod network;
#[cfg(test)]
mod tests;

pub(crate) use dispatch::FrameTracker;
use dispatch::{
  FrameContexts, Routes, dispatch_console, dispatch_dialog, handle_exec_context_created, handle_frame_attached,
  handle_frame_detached, handle_frame_navigated, handle_frame_navigated_within,
};
use interception::{dispatch_file_chooser, handle_request_intercepted};
use network::{
  WebSockets, handle_loading_failed, handle_loading_finished, handle_request_will_be_sent, handle_response_received,
  handle_websocket_closed, handle_websocket_created, handle_websocket_error, handle_websocket_frame,
};
