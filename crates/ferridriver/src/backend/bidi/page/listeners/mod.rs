//! `BidiPage` event listener loop, network tracker, and route execution.

use super::{
  Arc, BINDING_CHANNEL, BidiPage, BidiSession, ConsoleMessage, DialogEvent, EventEmitter, FxHashMap, NetworkRequest,
  Ordering, PageEvent, RwLock, bidi_remote_value_to_backing, bidi_stack_to_location, bidi_stack_trace_to_location,
  build_bidi_stack, f64_to_u64_saturating, json, split_error_text,
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

mod route_actions;
mod tracker;

pub(crate) use route_actions::NetworkSettings;
pub(super) use route_actions::{
  color_scheme_feature, download_behavior_params, execute_bidi_route_action, parse_bidi_cookie,
};
use tracker::BidiNetworkTracker;
pub(super) use tracker::{parse_bidi_headers, permissions_origin};
