//! `BidiPage` event listener loop, network tracker, and route execution.

use super::{
  Arc, BINDING_CHANNEL, BidiPage, BidiSession, BodyFn, ConsoleMessage, CookieData, DialogEvent, Engine, EventEmitter,
  FerriError, FxHashMap, NetworkRequest, Ordering, PageEvent, RemoteAddr, RequestInit, Response, ResponseInit, Result,
  RwLock, SecurityDetails, bidi_remote_value_to_backing, bidi_stack_to_location, bidi_stack_trace_to_location,
  build_bidi_stack, f64_to_u64_saturating, json, network, split_error_text,
};

impl BidiPage {
  // ── Event listeners ─────────────────────────────────────────────────────

  // The BiDi listener branches on every protocol event we subscribe
  // to (network, console, dialog, navigation) and has grown past
  // clippy's default 100-line cap. Splitting it into per-domain
  // sub-functions would require threading eight captured `Arc`s
  // through each call — the inline match is clearer as-is.
  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
  ) {
    self.shared.register_emitter_bridges();

    // Configure Firefox to land downloads in our per-page tempdir.
    // `browser.setDownloadBehavior` is a best-effort browser-scoped
    // command (Playwright swallows errors too); the tempdir drop
    // cleans up any files if the command fails. Firing the setup on a
    // detached task because `attach_listeners` is synchronous.
    {
      let session = Arc::clone(&self.session);
      let downloads_subdir = Arc::clone(&self.downloads_subdir);
      let accept_downloads = Arc::clone(&self.accept_downloads);
      let user_context = session.user_contexts.then(|| Arc::clone(&self.user_context));
      tokio::spawn(async move {
        let _ = session
          .transport
          .send_command(
            "browser.setDownloadBehavior",
            download_behavior_params(&accept_downloads, &downloads_subdir, user_context.as_deref()),
          )
          .await;
      });
    }

    self.spawn_worker_listener();
    self
      .session
      .transport
      .observe_events(super::super::frames::frame_observer(
        Arc::clone(&self.context_id),
        Arc::clone(&self.frame_cache),
        Arc::clone(&self.closed),
      ));
    let rx = self.session.transport.tap_events();
    *self
      .event_barrier
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rx.barrier());
    let tracker = Arc::new(BidiNetworkTracker::new(
      Arc::clone(&self.session),
      Arc::clone(&self.context_id),
      self.nav_request_slot.clone(),
      Arc::clone(&self.http_credentials),
      Arc::clone(&self.network),
    ));
    let (binding_tx, mut binding_rx) =
      tokio::sync::mpsc::unbounded_channel::<futures::future::BoxFuture<'static, ()>>();
    let binding_worker = tokio::spawn(async move {
      while let Some(call) = binding_rx.recv().await {
        call.await;
      }
    });
    self.track_listener(binding_worker.abort_handle());

    let event_loop = PageEventLoop {
      page: self.clone(),
      console_log,
      network_log,
      dialog_log,
      tracker,
      binding_tx,
      child_frames: FxHashMap::default(),
    };
    let main_listener = tokio::spawn(event_loop.run(rx));
    self.track_listener(main_listener.abort_handle());
  }
}

/// The session-wide event loop `BidiPage::attach_listeners` runs for one
/// page: what it scopes to the page's frame tree and the logs it feeds.
pub(super) struct PageEventLoop {
  page: BidiPage,
  console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
  network_log: Arc<RwLock<Vec<NetworkRequest>>>,
  dialog_log: Arc<RwLock<Vec<DialogEvent>>>,
  tracker: Arc<BidiNetworkTracker>,
  binding_tx: tokio::sync::mpsc::UnboundedSender<futures::future::BoxFuture<'static, ()>>,
  /// Child browsing contexts (iframes) of this page, tracked from
  /// `contextCreated` / `contextDestroyed`. Used to scope events that
  /// only carry a `source.context` (exposed-binding dispatch via
  /// `log.entryAdded`) to this page's frame tree.
  /// Each child frame, at any depth, and the frame it is in.
  child_frames: FxHashMap<String, String>,
}

fn param_str<'a>(params: &'a serde_json::Value, key: &str) -> &'a str {
  params.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

impl PageEventLoop {
  async fn run(mut self, mut rx: super::super::transport::EventTap) {
    while let Some(event) = rx.recv().await {
      if self.handle(&event).await.is_break() {
        break;
      }
    }
  }

  async fn handle(&mut self, event: &super::super::transport::BidiEvent) -> std::ops::ControlFlow<()> {
    use std::ops::ControlFlow;
    let ctx: &str = &self.page.context_id;
    // Filter events for this context. Accepts events targeting this
    // top-level context directly, events with no `context` field
    // (session-wide), and events whose `parent` matches us — the
    // latter covers `browsingContext.contextCreated` for child
    // iframes that BiDi reports under the child's own id.
    let event_ctx = param_str(&event.params, "context");
    let event_parent = param_str(&event.params, "parent");
    // A frame inside a frame names its own parent, which is one of ours.
    let child_of_this =
      !event_parent.is_empty() && (event_parent == ctx || self.child_frames.contains_key(event_parent));
    if event_ctx != ctx && !event_ctx.is_empty() && !child_of_this && !self.child_frames.contains_key(event_ctx) {
      return ControlFlow::Continue(());
    }

    if event.method == "browsingContext.contextCreated" && child_of_this {
      self.on_child_context_created(event_ctx, event_parent, param_str(&event.params, "url"));
      return ControlFlow::Continue(());
    }

    match event.method.as_str() {
      // Same-document navigations: `historyUpdated` covers
      // `history.pushState` / `replaceState`, `fragmentNavigated`
      // covers hash changes. The document (and `window.__fd`)
      // persists, so no engine reset — just keep the tracked URL
      // fresh. Mirrors Playwright's `bidiPage.ts::_onHistoryUpdated`
      // / `_onFragmentNavigated`.
      "browsingContext.historyUpdated" | "browsingContext.fragmentNavigated" => {
        if let Some(frame) = self.frame_info(event_ctx, param_str(&event.params, "url")) {
          self.page.events.emit(PageEvent::FrameNavigatedWithinDocument(frame));
        }
      },
      "browsingContext.navigationStarted"
      | "browsingContext.navigationCommitted"
      | "browsingContext.domContentLoaded"
      | "browsingContext.load" => self.on_navigation(event, event_ctx).await,
      "script.message" => self.on_script_message(&event.params).await,
      "log.entryAdded" => self.on_log_entry(&event.params).await,
      "network.beforeRequestSent" => {
        self
          .tracker
          .on_before_request_sent(&event.params, &self.page.events, &self.network_log)
          .await;
      },
      "network.responseStarted" => {
        self.tracker.on_response_started(&event.params, &self.page.events).await;
      },
      "network.responseCompleted" => {
        self
          .tracker
          .on_response_completed(&event.params, &self.page.events)
          .await;
      },
      "network.fetchError" => {
        self.tracker.on_fetch_error(&event.params, &self.page.events).await;
      },
      "network.authRequired" => {
        self.tracker.on_auth_required(&event.params).await;
      },
      "browsingContext.userPromptOpened" => self.on_user_prompt_opened(&event.params).await,
      // Playwright's `_onUserPromptClosed`: a prompt the browser
      // closed without us still closes the dialog it opened.
      "browsingContext.userPromptClosed" => self.page.dialog_manager.closed_in_browser(),
      "browsingContext.contextDestroyed" => {
        if event_ctx == ctx {
          // `swap` dedupes against `close_page`, which also emits
          // `Close` (it can't rely on this event still being
          // routed once the listener is aborted).
          if !self.page.closed.swap(true, Ordering::SeqCst) {
            self.page.events.emit(PageEvent::Close);
          }
          // This page is gone — stop consuming session-wide events.
          return ControlFlow::Break(());
        }
        // A child iframe was removed — that's a frame detach, not
        // a page close (treating it as Close bricked the page and
        // tore down every page-scoped consumer).
        self.child_frames.remove(event_ctx);
        self.page.events.emit(PageEvent::FrameDetached {
          frame_id: event_ctx.to_string(),
        });
      },
      "input.fileDialogOpened" => self.on_file_dialog_opened(&event.params),
      "browsingContext.downloadWillBegin" => self.on_download_will_begin(&event.params),
      "browsingContext.downloadEnd" => self.on_download_end(&event.params),
      _ => {},
    }
    ControlFlow::Continue(())
  }

  /// The main frame or a tracked child frame `frame_id` at `url`, or
  /// `None` for a context outside this page.
  fn frame_info(&self, frame_id: &str, url: &str) -> Option<crate::backend::FrameInfo> {
    let parent_frame_id = if frame_id == &*self.page.context_id {
      None
    } else {
      Some(self.child_frames.get(frame_id)?.clone())
    };
    Some(crate::backend::FrameInfo {
      frame_id: frame_id.to_string(),
      parent_frame_id,
      name: String::new(),
      url: url.to_string(),
    })
  }

  /// `browsingContext.contextCreated` events for child iframes
  /// surface here with their own (child) `context` id and the
  /// top-level `parent` set to our context. Emit a `FrameAttached`
  /// so the page-level frame cache (used by `page.frame(name)`,
  /// `page.frames()`) sees the iframe. Without this `BiDi` pages
  /// expose no child-frame metadata at all.
  ///
  /// `BiDi`'s `contextCreated` payload does not carry the
  /// iframe's `name` attribute. Spawn a follow-up
  /// `browsingContext.getTree` to enrich the cache record with
  /// name + final url once Firefox has wired the iframe up.
  fn on_child_context_created(&mut self, frame_id: &str, parent_id: &str, url: &str) {
    self.child_frames.insert(frame_id.to_string(), parent_id.to_string());
    self
      .page
      .events
      .emit(PageEvent::FrameAttached(crate::backend::FrameInfo {
        frame_id: frame_id.to_string(),
        parent_frame_id: Some(parent_id.to_string()),
        name: String::new(),
        url: url.to_string(),
      }));
    // Async refresh: fetch the iframe's `name` (and updated `url`)
    // via `browsingContext.getTree`. Firefox populates `name` once
    // the iframe element is parsed; we re-emit a `FrameNavigated`
    // with the enriched info so the cache lookup picks up
    // `page.frame('target')` matches.
    tokio::spawn(refresh_child_frame(
      Arc::clone(&self.page.session),
      self.page.events.clone(),
      Arc::clone(&self.page.frame_cache),
      parent_id.to_string(),
      frame_id.to_string(),
    ));
  }

  async fn on_navigation(&self, event: &super::super::transport::BidiEvent, event_ctx: &str) {
    // Materialise context-wide permission grants for the newly
    // navigated origin (mirrors Playwright's
    // `doGrantGlobalPermissionsForURL` on navigation commit).
    // Awaited inline so the grant lands before this same loop
    // emits `load` — `goto` resolves on that event, so page
    // code observing the document never races the grant. The
    // helper sends nothing when the origin is already granted.
    if event.method == "browsingContext.navigationStarted" {
      let nav_url = param_str(&event.params, "url");
      if let Err(e) = self.page.grant_global_permissions_for_url(nav_url).await {
        tracing::debug!("permissions grant for {nav_url} failed: {e}");
      }
    }
    // Surface the main frame's cross-document navigation as
    // `FrameNavigated` (CDP gets this from `Page.frameNavigated`;
    // BiDi has no direct analogue). Keeps the frame cache's
    // main-frame URL fresh and starts a new `since-navigation`
    // window for `consoleMessages()` / `pageErrors()`. Only the
    // `navigationCommitted` arm fires — marking again at
    // `domContentLoaded` / `load` would wrongly evict console
    // output from inline scripts that ran during parse.
    //
    // A child-iframe cross-document navigation surfaces the same way,
    // like CDP's per-frame `Page.frameNavigated`, so frame-scoped
    // consumers (e.g. WebSocket-route teardown) observe it.
    if event.method == "browsingContext.navigationCommitted"
      && let Some(frame) = self.frame_info(event_ctx, param_str(&event.params, "url"))
    {
      self.page.events.emit(PageEvent::FrameNavigated(frame));
    }
    // Surface the main frame's lifecycle on the page emitter so
    // `page.on('load' | 'domcontentloaded')` observes it.
    if event_ctx == &*self.page.context_id {
      match event.method.as_str() {
        "browsingContext.domContentLoaded" => self.page.events.emit(PageEvent::DomContentLoaded),
        "browsingContext.load" => self.page.events.emit(PageEvent::Load),
        _ => {},
      }
    }
    // Scope the engine-state reset to the context that actually
    // navigated. A child iframe reloading must not wipe the
    // main context's `window.__fd` (and vice-versa) — that
    // blanket reset was forcing a redundant re-inject every
    // time any frame fired a lifecycle event (Residual 2).
    self.page.injected_script.reset_context(event_ctx);
  }

  /// Exposed-binding dispatch. The shim installed by
  /// `expose_binding` sends `JSON.stringify({name, id, args})`
  /// through the `BINDING_CHANNEL` channel; the payload
  /// arrives as a string `RemoteValue` in `data` and
  /// `source.context` names the calling frame (an iframe
  /// caller surfaces its own frame, and the promise resolve
  /// below must target that same context). Mirrors
  /// Playwright's `bidiPage.ts::_onScriptMessage`.
  async fn on_script_message(&self, params: &serde_json::Value) {
    if params.get("channel").and_then(|v| v.as_str()) != Some(BINDING_CHANNEL) {
      return;
    }
    let Some(text) = params.get("data").and_then(|d| d.get("value")).and_then(|v| v.as_str()) else {
      return;
    };
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(text) else {
      return;
    };
    let fn_name = param_str(&payload, "name").to_string();
    let id = param_str(&payload, "id").to_string();
    let args: Vec<serde_json::Value> = payload
      .get("args")
      .and_then(|v| v.as_array())
      .cloned()
      .unwrap_or_default();
    let page_ctx = &*self.page.context_id;
    let call_ctx = params
      .get("source")
      .and_then(|s| s.get("context"))
      .and_then(|v| v.as_str())
      .unwrap_or(page_ctx)
      .to_string();
    // `script.message` has no top-level `context`, so the page
    // filter above can't scope it — every page's listener on
    // this session sees every envelope. Only dispatch calls
    // originating in THIS page's frame tree; otherwise a
    // same-named binding on another page would double-fire.
    if call_ctx != page_ctx && !self.child_frames.contains_key(&call_ctx) {
      return;
    }
    let source = crate::events::BindingSource {
      context: String::new(),
      page: page_ctx.to_string(),
      frame: call_ctx.clone(),
      live: None,
    };
    let maybe_fn = self.page.exposed_fns.read().await.get(&fn_name).cloned();
    let exposed_session = Arc::clone(&self.page.session);
    let _ = self.binding_tx.send(Box::pin(async move {
      let outcome = match maybe_fn {
        Some(callback) => callback(source, args).await,
        None => Err(format!("Function not found: {fn_name}")),
      };
      // The value is an expression evaluated in the calling
      // context, only once its parked promise is found there.
      let (settled, expression, message) = match outcome {
        Ok(expression) => (true, expression, String::new()),
        Err(message) => (false, String::from("undefined"), message),
      };
      let _ = exposed_session
        .transport
        .send_command(
          "script.callFunction",
          json!({
            "functionDeclaration": format!("(id, ok, message) => {{ const c = window.__ferri_exposed && window.__ferri_exposed[id]; if (c) {{ delete window.__ferri_exposed[id]; ok ? c.resolve({expression}) : c.reject(new Error(message)); }} }}"),
            "arguments": [
              {"type": "string", "value": id},
              {"type": "boolean", "value": settled},
              {"type": "string", "value": message},
            ],
            "target": {"context": call_ctx},
            "awaitPromise": false,
            "resultOwnership": "none"
          }),
        )
        .await;
    }));
  }

  /// Mirrors Playwright's `bidiPage.ts::_onLogEntryAdded`.
  /// Routes `type: 'javascript'` + `level: 'error'` entries to
  /// `PageEvent::PageError(WebError)`; `type: 'console'`
  /// entries to `PageEvent::Console(ConsoleMessage)`; other
  /// entry types (`'deprecation'`, etc.) are ignored.
  async fn on_log_entry(&self, params: &serde_json::Value) {
    let entry_type = param_str(params, "type");
    let level = param_str(params, "level");
    if entry_type == "javascript" && level == "error" {
      // Parity with `bidiPage.ts:267-286`: split `text` at the
      // first `': '` for `name` / `message`, synthesise `stack`
      // from the entry's `stackTrace.callFrames` (BiDi
      // line/column are 0-based so Playwright adds `+ 1`).
      let text = param_str(params, "text");
      let (name, message) = split_error_text(text);
      let stack = build_bidi_stack(text, params.get("stackTrace"));
      let details = crate::web_error::ErrorDetails { name, message, stack };
      let location = bidi_stack_to_location(params.get("stackTrace"));
      crate::backend::shared_page::dispatch_page_error_event(
        &self.page.page_backref,
        &self.page.events,
        details,
        location,
      );
      return;
    }
    if entry_type != "console" {
      return;
    }

    // BiDi reports `method` for the console variant (e.g.
    // 'log', 'warn', 'error'). Playwright remaps `'warn'` to
    // `'warning'` for parity with CDP's `Runtime.consoleAPICalled.type`.
    let method = params.get("method").and_then(|v| v.as_str()).unwrap_or("log");
    let type_str = if method == "warn" { "warning" } else { method };
    // `text` is explicit only for `timeLog` / `timeEnd`. For
    // other variants Playwright leaves it undefined so the
    // lazy `text()` falls back to args preview.
    let text = if matches!(method, "timeLog" | "timeEnd") {
      params
        .get("text")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
    } else {
      None
    };
    let arg_backings = params
      .get("args")
      .and_then(|v| v.as_array())
      .map(Vec::as_slice)
      .unwrap_or_default()
      .iter()
      .map(|arg| {
        let backing = bidi_remote_value_to_backing(arg);
        let is_node = arg.get("type").and_then(|v| v.as_str()) == Some("node");
        (backing, is_node)
      })
      .collect();
    let location = bidi_stack_trace_to_location(params.get("stackTrace"), params.get("source"));
    let timestamp = params
      .get("timestamp")
      .and_then(serde_json::Value::as_f64)
      .map_or(0, f64_to_u64_saturating);
    crate::backend::shared_page::dispatch_console_event(
      &self.page.page_backref,
      &self.console_log,
      &self.page.events,
      crate::backend::shared_page::ConsoleEventInput {
        kind: type_str.to_string(),
        explicit_text: text,
        arg_backings,
        location,
        timestamp,
      },
    )
    .await;
  }

  async fn on_user_prompt_opened(&self, params: &serde_json::Value) {
    let Ok(payload) =
      serde_json::from_value::<ferridriver_bidi_proto::browsing_context::UserPromptOpenedEvent>(params.clone())
    else {
      return;
    };
    let prompt_type_str = if payload.prompt_type.is_empty() {
      "alert".to_string()
    } else {
      payload.prompt_type
    };
    let message = payload.message;
    let default_value = payload.default_value;

    // Build the responder: translates the user's accept/dismiss
    // into `browsingContext.handleUserPrompt`. Captures an Arc
    // of the session + context id.
    let responder_session = Arc::clone(&self.page.session);
    let responder_ctx = Arc::clone(&self.page.context_id);
    let responder: crate::dialog::DialogResponder = Arc::new(move |response| {
      let session = Arc::clone(&responder_session);
      let ctx = Arc::clone(&responder_ctx);
      Box::pin(async move {
        let (accept, user_text) = match response {
          crate::dialog::DialogResponse::Accept { prompt_text } => (true, prompt_text),
          crate::dialog::DialogResponse::Dismiss => (false, None),
        };
        session
          .transport
          .send(&ferridriver_bidi_proto::browsing_context::HandleUserPrompt {
            context: &ctx,
            accept: Some(accept),
            user_text: user_text.as_deref(),
          })
          .await
          .map(|_| ())
      })
    });

    crate::backend::shared_page::dispatch_dialog_event(
      &self.page.page_backref,
      &self.page.dialog_manager,
      &self.dialog_log,
      prompt_type_str,
      message,
      default_value,
      responder,
    )
    .await;
  }

  /// `BiDi` event shape:
  /// { "context": "<frameId>", "element": { "sharedId": "..." }, "multiple": bool }
  /// Per `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiPage.ts::_onFileDialogOpened`.
  fn on_file_dialog_opened(&self, params: &serde_json::Value) {
    let Ok(payload) = serde_json::from_value::<ferridriver_bidi_proto::input::FileDialogOpenedEvent>(params.clone())
    else {
      return;
    };
    let Some(shared_id) = payload.element.and_then(|e| e.shared_id) else {
      return;
    };
    let is_multiple = payload.multiple;
    // Resolve the element -> ElementHandle off the hot
    // subscription loop. Same rationale as the CDP listener:
    // we don't want rapid file-picker triggers queued
    // behind a slow DOM round-trip.
    let manager = self.page.file_chooser_manager.clone();
    let backref = self.page.page_backref.clone();
    let ctx = Arc::clone(&self.page.context_id);
    let session = Arc::clone(&self.page.session);
    tokio::spawn(async move {
      let Some(page) = backref.upgrade() else {
        return;
      };
      let element = crate::backend::AnyElement::Bidi(super::super::BidiElement::new(session, ctx, shared_id));
      crate::backend::shared_page::dispatch_file_chooser_event(page, &manager, element, is_multiple).await;
    });
  }

  /// `BiDi` correlates a download with its triggering navigation
  /// via the `navigation` id on the event. If the browser
  /// doesn't attribute it to a navigation, Playwright skips
  /// (see `bidiPage.ts::_onDownloadWillBegin`) — so do we.
  fn on_download_will_begin(&self, params: &serde_json::Value) {
    let Ok(payload) =
      serde_json::from_value::<ferridriver_bidi_proto::browsing_context::DownloadWillBeginEvent>(params.clone())
    else {
      return;
    };
    let Some(navigation) = payload.navigation else {
      return;
    };
    let url = payload.url;
    let suggested = payload.suggested_filename;

    // BiDi has no native cancel — Playwright's own
    // `bidiBrowser.ts::cancelDownload` is a no-op. Surface
    // typed `Unsupported` so callers know.
    let canceler: crate::download::DownloadCanceler = Arc::new(|| {
      Box::pin(async {
        Err(crate::error::FerriError::Unsupported(
          "Download.cancel is not supported on the BiDi backend: Firefox's BiDi implementation has no browser.cancelDownload command and Playwright's own BiDi backend leaves cancelDownload as a no-op (see bidiBrowser.ts::cancelDownload)".into(),
        ))
      })
    });

    crate::backend::shared_page::dispatch_download_started(
      &self.page.page_backref,
      &self.page.download_manager,
      navigation,
      url,
      suggested,
      self.page.downloads_subdir.to_path_buf(),
      canceler,
    );
  }

  fn on_download_end(&self, params: &serde_json::Value) {
    let Ok(payload) =
      serde_json::from_value::<ferridriver_bidi_proto::browsing_context::DownloadEndEvent>(params.clone())
    else {
      return;
    };
    let Some(navigation) = payload.navigation.as_deref() else {
      return;
    };
    let status = if payload.status.is_empty() {
      "complete"
    } else {
      payload.status.as_str()
    };
    let download_manager = &self.page.download_manager;
    if let Some(d) = download_manager.take_for_guid(navigation) {
      if status == "canceled" {
        download_manager.report_canceled(&d, "canceled");
      } else {
        // `complete` carries the absolute `filepath` Firefox
        // wrote to — override the default
        // `<downloads_dir>/<guid>` path with the real one.
        let filepath = payload.filepath.map(std::path::PathBuf::from);
        d.report_finished(filepath, None);
      }
    }
  }
}

/// Re-read child frame `child_id` of `parent_id` with
/// `browsingContext.getTree` and record the name Firefox filled in.
async fn refresh_child_frame(
  session: Arc<BidiSession>,
  emitter: EventEmitter,
  frame_cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  parent_id: String,
  child_id: String,
) {
  let result = session
    .transport
    .send_command("browsingContext.getTree", json!({"root": &parent_id, "maxDepth": 1}))
    .await;
  let Ok(tree) = result else { return };
  let Some(contexts) = tree.get("contexts").and_then(|v| v.as_array()) else {
    return;
  };
  // The returned array has the parent at index 0; its
  // `children` array contains the iframe entries with `name`.
  let Some(children) = contexts
    .first()
    .and_then(|p| p.get("children"))
    .and_then(|v| v.as_array())
  else {
    return;
  };
  let Some(child) = children.iter().find(|child| param_str(child, "context") == child_id) else {
    return;
  };
  let cid = param_str(child, "context");
  let cname = param_str(child, "name").to_string();
  let curl = param_str(child, "url").to_string();
  // Only the name: the frame may have navigated since the
  // tree was read, and the reader keeps its URL in wire order.
  {
    let mut cache = frame_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.record(cid).is_some_and(|record| !record.detached) {
      cache.attach(crate::backend::FrameInfo {
        frame_id: cid.to_string(),
        parent_frame_id: Some(parent_id.clone()),
        name: cname.clone(),
        url: String::new(),
      });
    }
  }
  emitter.emit(PageEvent::FrameNavigated(crate::backend::FrameInfo {
    frame_id: cid.to_string(),
    parent_frame_id: Some(parent_id),
    name: cname,
    url: curl,
  }));
}

/// Per-page bookkeeping for live `Request` / `Response` lifecycle objects
/// across the `BiDi` network event sequence (`beforeRequestSent` →
/// `responseStarted` → `responseCompleted` / `fetchError`).
///
/// W3C `BiDi` today does not expose `WebSocket` frame events — Playwright's
/// own `BiDi` backend skips `WebSocket` handling for the same reason — so
/// `WebSocket.body()` / `WebSocket` frame observation surface the typed
/// `FerriError::Unsupported` per Rule 4 instead of dangling indefinitely.
pub(super) struct BidiNetworkTracker {
  session: Arc<super::super::session::BidiSession>,
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
  fn new(
    session: Arc<super::super::session::BidiSession>,
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

  async fn on_before_request_sent(
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
    let headers = req.get("headers").map(parse_bidi_headers).unwrap_or_default();
    let resource_type = params
      .get("initiator")
      .and_then(|i| i.get("type"))
      .and_then(|v| v.as_str())
      .map_or("", |t| match t {
        "parser" => "Document",
        "script" => "Script",
        "preflight" => "Preflight",
        other => other,
      })
      .to_string();
    let post_data = req
      .get("body")
      .and_then(|b| b.get("value"))
      .and_then(|v| v.as_str())
      .map(|s| s.as_bytes().to_vec());
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
      raw_headers_fn: None,
    });

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

  async fn on_response_started(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
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

  async fn on_response_completed(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
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
  async fn on_auth_required(self: &Arc<Self>, params: &serde_json::Value) {
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

  async fn on_fetch_error(self: &Arc<Self>, params: &serde_json::Value, emitter: &EventEmitter) {
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
pub(super) fn permissions_origin(url: &str) -> Option<String> {
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

/// Parse BiDi-format headers `[{name, value: {type, value}}]` into a `FxHashMap`.
pub(super) fn parse_bidi_headers(headers_val: &serde_json::Value) -> FxHashMap<String, String> {
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

/// Execute a route action via `BiDi` network commands.
pub(super) async fn execute_bidi_route_action(
  transport: &super::super::transport::BidiTransport,
  request_id: &str,
  action: crate::route::RouteAction,
  orig_method: &str,
  orig_headers: &FxHashMap<String, String>,
  network: &NetworkSettings,
) {
  match action {
    crate::route::RouteAction::Fulfill(resp) => {
      let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp.body);
      let mut hdrs: Vec<serde_json::Value> = resp
        .headers
        .iter()
        .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
        .collect();
      if let Some(ct) = &resp.content_type
        && !hdrs
          .iter()
          .any(|h| h.get("name").and_then(|n| n.as_str()) == Some("content-type"))
      {
        hdrs.push(json!({"name": "content-type", "value": {"type": "string", "value": ct}}));
      }
      let _ = transport
        .send_command(
          "network.provideResponse",
          json!({
            "request": request_id,
            "statusCode": resp.status,
            "reasonPhrase": crate::route::status_text(resp.status),
            "headers": hdrs,
            "body": {"type": "base64", "value": body_b64},
          }),
        )
        .await;
    },
    crate::route::RouteAction::Continue(overrides) | crate::route::RouteAction::Fallback(overrides) => {
      if let Some(target_url) = &overrides.url {
        // Continue with a URL override. Firefox/BiDi `network.continueRequest`
        // accepts a `url` field but then aborts the request instead of
        // honoring it (verified against Firefox; Playwright sends it
        // best-effort and it is silently dropped). Implement the rewrite
        // faithfully: issue the request to the new URL ourselves — applying
        // the method/header/body overrides on top of the original request's
        // method/headers (so cookies and auth carry over) — and fulfill the
        // ORIGINAL request with that response. The page sees its own request
        // resolve to the new URL's content: `response.url` stays the original
        // and `response.redirected` is false, exactly like a native continue
        // with a rewritten URL.
        let method = overrides.method.as_deref().unwrap_or(orig_method);
        // Original headers, then apply the override headers on top.
        let mut merged: Vec<(String, String)> = orig_headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Some(over) = &overrides.headers {
          merged.retain(|(k, _)| !over.iter().any(|(ok, _)| ok.eq_ignore_ascii_case(k)));
          merged.extend(over.iter().cloned());
        }
        bidi_continue_via_fetch(
          transport,
          request_id,
          RouteRefetch {
            url: target_url,
            method,
            headers: &merged,
            body: overrides.post_data.as_deref(),
            network,
          },
        )
        .await;
      } else {
        let mut params = json!({"request": request_id});
        if let Some(method) = &overrides.method {
          params["method"] = serde_json::Value::String(method.clone());
        }
        if let Some(headers) = &overrides.headers {
          let hdrs: Vec<serde_json::Value> = headers
            .iter()
            .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
            .collect();
          params["headers"] = serde_json::Value::Array(hdrs);
        }
        if let Some(post_data) = &overrides.post_data {
          let encoded = base64::engine::general_purpose::STANDARD.encode(post_data);
          params["body"] = json!({"type": "base64", "value": encoded});
        }
        let _ = transport.send_command("network.continueRequest", params).await;
      }
    },
    crate::route::RouteAction::Abort(_reason) => {
      let _ = transport
        .send_command("network.failRequest", json!({"request": request_id}))
        .await;
    },
    // Chain-internal marker; never emitted as a terminal action. Fail
    // open like an unhandled route if it ever leaks.
    crate::route::RouteAction::PredicateRejected => {
      let _ = transport
        .send_command("network.continueRequest", json!({"request": request_id}))
        .await;
    },
  }
}

/// A route's `continue({ url })` as this backend re-issues it.
struct RouteRefetch<'a> {
  url: &'a str,
  method: &'a str,
  headers: &'a [(String, String)],
  body: Option<&'a [u8]>,
  network: &'a NetworkSettings,
}

/// Continue an intercepted request with a `url` override on `BiDi` by issuing
/// the request to the new URL ourselves and fulfilling the ORIGINAL request
/// with the result — Firefox's `network.continueRequest` `url` field aborts
/// rather than rewriting. The page's request resolves to the new URL's
/// content (`response.url` stays original, not a redirect). On any failure
/// the original request is continued unmodified so the page never stalls.
async fn bidi_continue_via_fetch(
  transport: &super::super::transport::BidiTransport,
  request_id: &str,
  refetch: RouteRefetch<'_>,
) {
  match fetch_for_route(&refetch).await {
    Ok((status, resp_headers, resp_body)) => {
      let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp_body);
      let hdrs: Vec<serde_json::Value> = resp_headers
        .iter()
        .map(|(k, v)| json!({"name": k, "value": {"type": "string", "value": v}}))
        .collect();
      let _ = transport
        .send_command(
          "network.provideResponse",
          json!({
            "request": request_id,
            "statusCode": status,
            "reasonPhrase": crate::route::status_text(i32::from(status)),
            "headers": hdrs,
            "body": {"type": "base64", "value": body_b64},
          }),
        )
        .await;
    },
    Err(e) => {
      tracing::warn!("bidi route url-override fetch failed ({e}); continuing request unmodified");
      let _ = transport
        .send_command("network.continueRequest", json!({"request": request_id}))
        .await;
    },
  }
}

/// Issue the HTTP request for a route `url` override, through the context's
/// proxy and with its TLS posture, as the browser would have sent it.
/// Hop-by-hop / length headers are dropped (the client sets them on the
/// request; the browser recomputes them when the fulfilled body is
/// delivered).
async fn fetch_for_route(refetch: &RouteRefetch<'_>) -> Result<(u16, Vec<(String, String)>, Vec<u8>)> {
  let headers: Vec<(String, String)> = refetch
    .headers
    .iter()
    .filter(|(k, _)| {
      !matches!(
        k.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "connection" | "accept-encoding"
      )
    })
    .cloned()
    .collect();
  let client = crate::http_client::HttpClient::new(crate::http_client::HttpClientOptions {
    ignore_https_errors: refetch.network.ignore_https_errors,
    proxy: refetch.network.proxy.clone(),
    ..Default::default()
  });
  let response = client
    .fetch(refetch.url)
    .options(crate::http_client::RequestOptions {
      method: Some(refetch.method.to_string()),
      headers: Some(headers),
      data: refetch.body.map(<[u8]>::to_vec),
      fail_on_status_code: Some(false),
      ..Default::default()
    })
    .await
    .map_err(|e| FerriError::protocol("route url override", format!("request to {} failed: {e}", refetch.url)))?;

  let status = response.status();
  let resp_headers: Vec<(String, String)> = response
    .headers()
    .iter()
    .filter(|(k, _)| {
      !matches!(
        k.to_ascii_lowercase().as_str(),
        "content-length" | "transfer-encoding" | "connection" | "content-encoding"
      )
    })
    .cloned()
    .collect();
  Ok((status, resp_headers, response.body()?.to_vec()))
}

/// The proxy and TLS posture a page's context was made with.
#[derive(Debug, Clone, Default)]
pub(crate) struct NetworkSettings {
  pub(crate) proxy: Option<crate::options::ProxyConfig>,
  pub(crate) ignore_https_errors: bool,
}

/// One attempt per request and per source: a proxy's challenge and then the
/// site's for the same request are two attempts, as in Playwright's
/// `authAttemptKey`.
fn auth_attempt_key(request: &str, from_proxy: bool) -> String {
  format!("{request}:{}", if from_proxy { "proxy" } else { "server" })
}

fn forget_auth_attempts(attempted: &mut rustc_hash::FxHashSet<String>, request: &str) {
  attempted.remove(&auth_attempt_key(request, true));
  attempted.remove(&auth_attempt_key(request, false));
}

/// Parse a `BiDi` network cookie into our `CookieData`.
pub(super) fn parse_bidi_cookie(c: &serde_json::Value) -> CookieData {
  let name = c.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let value = c
    .get("value")
    .and_then(|v| {
      // BiDi cookies have {type: "string", value: "..."} format
      v.get("value").and_then(|inner| inner.as_str()).or_else(|| v.as_str())
    })
    .unwrap_or("")
    .to_string();
  let domain = c.get("domain").and_then(|v| v.as_str()).unwrap_or("").to_string();
  let path = c.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string();
  let secure = c.get("secure").and_then(serde_json::Value::as_bool).unwrap_or(false);
  let http_only = c.get("httpOnly").and_then(serde_json::Value::as_bool).unwrap_or(false);
  let expires = c.get("expiry").and_then(serde_json::Value::as_f64);
  let same_site = c.get("sameSite").and_then(|v| v.as_str()).and_then(|s| match s {
    "strict" => Some(crate::backend::SameSite::Strict),
    "lax" => Some(crate::backend::SameSite::Lax),
    "none" => Some(crate::backend::SameSite::None),
    _ => None,
  });

  CookieData {
    name,
    value,
    domain,
    path,
    secure,
    http_only,
    expires,
    same_site,
    url: None,
  }
}

/// The `prefers-color-scheme` value for `emulation.setMediaFeaturesOverride`:
/// the scheme, or `null` for the browser's own appearance (which, unlike
/// leaving the feature out, also sets aside a user context's override).
/// `None` when nothing is asked of it.
pub(super) fn color_scheme_feature(scheme: &crate::options::MediaOverride) -> Result<Option<serde_json::Value>> {
  use crate::options::MediaOverride;
  match scheme {
    MediaOverride::Unchanged => Ok(None),
    MediaOverride::Disabled => Ok(Some(serde_json::Value::Null)),
    MediaOverride::Set(value) if value == "light" || value == "dark" => Ok(Some(json!(value))),
    MediaOverride::Set(value) => Err(FerriError::unsupported(format!(
      "BiDi/Firefox emulates `colorScheme` as light or dark only, not `{value}`"
    ))),
  }
}

/// The one `browser.setDownloadBehavior` payload both senders use.
///
/// Firefox takes a destination folder only for the allowed case; a
/// `denied` behaviour carrying one is rejected.
pub(super) fn download_behavior_params(
  accept_downloads: &std::sync::atomic::AtomicBool,
  downloads_subdir: &std::path::Path,
  user_context: Option<&str>,
) -> serde_json::Value {
  let behavior = if accept_downloads.load(std::sync::atomic::Ordering::Relaxed) {
    serde_json::json!({
      "type": "allowed",
      "destinationFolder": downloads_subdir.to_string_lossy(),
    })
  } else {
    serde_json::json!({ "type": "denied" })
  };
  let mut params = serde_json::json!({ "downloadBehavior": behavior });
  if let Some(uc) = user_context {
    params["userContexts"] = serde_json::json!([uc]);
  }
  params
}
