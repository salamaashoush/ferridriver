//! `CdpPage` CDP event listener tasks (console, dialog, file chooser, download, network, frames).

use super::{
  Arc, BindingCall, CdpPage, CdpTransport, CdpWorker, ConsoleMessage, DownloadsDir, FxHashMap, NetworkRequest,
  NetworkTracker, RwLock, UTILITY_WORLD, cdp_exception_to_error_details, cdp_exception_to_location,
  cdp_remote_object_to_backing, cdp_stack_trace_to_location, f64_to_u64_saturating, transport,
};

impl CdpPage {
  // ---- Event listeners ----

  pub fn attach_listeners(
    &self,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    network_log: Arc<RwLock<Vec<NetworkRequest>>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    let transport = Arc::clone(&self.transport);
    let session_id = self.session_id.clone();
    let emitter1 = self.events.clone();
    let emitter2 = self.events.clone();
    let emitter3 = self.events.clone();
    let mut tasks: Vec<tokio::task::AbortHandle> = Vec::with_capacity(10);

    tasks.push(Self::spawn_console_listener(
      &transport,
      session_id.clone(),
      console_log,
      emitter1,
      self.page_backref.clone(),
    ));
    tasks.push(Self::spawn_web_error_listener(
      &transport,
      session_id.clone(),
      self.events.clone(),
      self.page_backref.clone(),
    ));
    tasks.extend(Self::spawn_lifecycle_event_listener(
      &transport,
      session_id.as_ref(),
      &self.events,
    ));
    tasks.push(Self::spawn_frame_name_listener(
      &transport,
      session_id.clone(),
      Arc::clone(&self.frame_cache),
    ));
    tasks.push(Self::spawn_network_listener(
      transport,
      session_id,
      Arc::clone(&self.target_id),
      network_log,
      emitter2,
      self.nav_request_slot.clone(),
    ));
    // Register the emitter-bridge: `page.events().on("dialog", cb)`
    // keeps working because the bridge handler — installed here for
    // the page's lifetime — synchronously claims dialogs on behalf
    // of broadcast listeners. Handler id is discarded: we never
    // unregister until the page is dropped.
    let _ = self.dialog_manager.register_emitter_bridge(self.events.clone());
    // Same pattern for file-chooser: broadcast `filechooser`
    // listeners see live handles via the bridge handler; one-shot
    // `page.wait_for_file_chooser` callers register directly.
    let _ = self.file_chooser_manager.register_emitter_bridge(self.events.clone());
    // And one more for downloads — live [`crate::download::Download`]
    // handles delivered to `page.events().on("download", cb)` via the
    // bridge, same claim-on-open pattern as dialog / file-chooser.
    let _ = self.download_manager.register_emitter_bridge(self.events.clone());

    tasks.push(Self::spawn_dialog_listener(
      Arc::clone(&self.transport),
      self.session_id.clone(),
      dialog_log,
      emitter3,
      self.dialog_manager.clone(),
      self.page_backref.clone(),
    ));
    tasks.push(Self::spawn_file_chooser_listener(
      &self.transport,
      self.session_id.clone(),
      self.file_chooser_manager.clone(),
      self.page_backref.clone(),
    ));
    tasks.push(Self::spawn_download_listener(
      Arc::clone(&self.transport),
      self.session_id.clone(),
      self.browser_context_id.clone(),
      self.download_manager.clone(),
      Arc::clone(&self.downloads_dir),
      self.page_backref.clone(),
    ));
    tasks.extend(self.spawn_renderer_tracker());
    tasks.push(self.spawn_target_gone_listener());
    tasks.push(self.spawn_crash_listener());
    tasks.push(self.spawn_worker_listener());

    if let Ok(mut guard) = self.listener_tasks.lock() {
      guard.extend(tasks);
    }
  }

  fn spawn_console_listener(
    transport: &Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    console_log: Arc<RwLock<Vec<ConsoleMessage>>>,
    emitter: crate::events::EventEmitter,
    page_backref: crate::backend::PageBackref,
  ) -> tokio::task::AbortHandle {
    // Tapped here, not in the task: the tap delivers only what arrives
    // after it exists, and a task is not polled the moment it is
    // spawned. An event in between would never be seen.
    let mut rx = transport.tap_event_methods(&["Runtime.consoleAPICalled"], session_id.as_deref());
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }

        let Some(params) = event.get("params") else {
          continue;
        };
        let Ok(payload) =
          serde_json::from_value::<ferridriver_cdp_proto::runtime::ConsoleApiCalledEvent>(params.clone())
        else {
          continue;
        };
        // Without an owning page we cannot build live `JSHandle`s —
        // drop the event silently (matches Playwright's
        // `createHandle(context, arg)` guard which returns early if the
        // execution context is unknown).
        let Some(page) = page_backref.upgrade() else {
          continue;
        };

        let type_str = if payload.kind.is_empty() {
          "log".to_string()
        } else {
          payload.kind
        };
        let mut args: Vec<crate::js_handle::JSHandle> = Vec::with_capacity(payload.args.len());
        for arg in &payload.args {
          let backing = cdp_remote_object_to_backing(arg, session_id.clone());
          let is_node = arg.get("subtype").and_then(|v| v.as_str()) == Some("node");
          args.push(crate::js_handle::JSHandle::from_backing(
            Arc::clone(&page),
            backing,
            is_node,
          ));
        }
        let location = cdp_stack_trace_to_location(payload.stack_trace.as_ref());
        let timestamp = f64_to_u64_saturating(payload.timestamp);

        let msg = crate::console_message::ConsoleMessage::new(&page, type_str, None, args, location, timestamp);
        crate::state::push_capped(
          &mut *console_log.write().await,
          msg.clone(),
          crate::state::CONSOLE_LOG_CAP,
        );
        emitter.emit(crate::events::PageEvent::Console(msg));
      }
    })
    .abort_handle()
  }

  /// Listen for `Runtime.exceptionThrown` and emit
  /// [`crate::events::PageEvent::PageError`] carrying a live
  /// [`crate::web_error::WebError`]. Mirrors Playwright's
  /// `crPage.ts:751` — `session.on('Runtime.exceptionThrown', …)` feeding
  /// `exceptionToError(exceptionDetails)` → `page.addPageError(err)`.
  ///
  /// The exception conversion follows
  /// `crProtocolHelper.ts::{getExceptionMessage, exceptionToError}`
  /// byte-for-byte: the combined `description` + stack lines is split at
  /// the first `    at …` line; everything before becomes
  /// `{ name, message }` (parsed via `splitErrorMessage`'s `': '`
  /// separator), everything from that line onward becomes `stack`. When
  /// the exception carries an object `preview` with a `name` property,
  /// that value overrides the parsed name — matches Playwright's
  /// `nameOverride` branch.
  /// Bridge CDP's `Page.loadEventFired` / `Page.domContentEventFired`
  /// onto the page emitter so `page.on('load' | 'domcontentloaded')`
  /// and `waitForEvent` observe them (Playwright parity).
  fn spawn_lifecycle_event_listener(
    transport: &Arc<CdpTransport>,
    session_id: Option<&Arc<str>>,
    emitter: &crate::events::EventEmitter,
  ) -> Vec<tokio::task::AbortHandle> {
    let transport = Arc::clone(transport);
    let session_id = session_id.cloned();
    let emitter = emitter.clone();
    // Tapped here, not in the task: the tap delivers only what arrives
    // after it exists, and a task is not polled the moment it is
    // spawned. An event in between would never be seen.
    let mut rx = transport.tap_event_methods(
      &["Page.loadEventFired", "Page.domContentEventFired"],
      session_id.as_deref(),
    );
    vec![
      tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
          if let Some(ref expected_sid) = session_id
            && event.get("sessionId").and_then(|v| v.as_str()) != Some(&**expected_sid)
          {
            continue;
          }
          let ev = match event.get("method").and_then(|m| m.as_str()) {
            Some("Page.loadEventFired") => crate::events::PageEvent::Load,
            Some("Page.domContentEventFired") => crate::events::PageEvent::DomContentLoaded,
            _ => continue,
          };
          emitter.emit(ev);
        }
      })
      .abort_handle(),
    ]
  }

  /// Mark the page closed when the browser destroys its target.
  ///
  /// `closed` was only ever set by our own `close_page` / `dispose_local`, so a
  /// target that went away on its own -- tab closed in the browser UI,
  /// `window.close()`, target detached or crashed -- left `is_closed()` false.
  /// The stale page then stayed the context's active page and every later
  /// command went to a dead CDP session, surfacing as
  /// `Session with given id not found` on some unrelated later call instead of
  /// as a lost page. Marking it here lets the context skip it and reopen.
  ///
  /// These are browser-level events (no `sessionId` filter), so the tap is
  /// unfiltered and matched on `targetId` instead.
  /// Dedicated workers auto-attach under the page's session. The
  /// browser-wide attach listener resumes them; this one announces each
  /// as a [`crate::worker::Worker`] and closes it when its session goes.
  fn spawn_worker_listener(&self) -> tokio::task::AbortHandle {
    let transport = Arc::clone(&self.transport);
    let emitter = self.events.clone();
    let mut rx = self.transport.tap_event_methods(
      &["Target.attachedToTarget", "Target.detachedFromTarget"],
      self.session_id.as_deref(),
    );
    tokio::spawn(async move {
      let mut workers: rustc_hash::FxHashMap<String, crate::worker::Worker> = rustc_hash::FxHashMap::default();
      while let Some(event) = rx.recv().await {
        let Some(params) = event.get("params") else {
          continue;
        };
        let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
        match ferridriver_cdp_proto::CdpEvent::parse(method, params) {
          Some(ferridriver_cdp_proto::CdpEvent::DetachedFromTarget(ev)) => {
            if let Some(worker) = workers.remove(&ev.session_id) {
              worker.did_close();
            }
          },
          Some(ferridriver_cdp_proto::CdpEvent::AttachedToTarget(ev)) if ev.target_info.target_type == "worker" => {
            let worker = crate::worker::Worker::new(
              ev.session_id.clone(),
              ev.target_info.url,
              Arc::new(CdpWorker {
                transport: Arc::clone(&transport),
                session_id: Arc::from(ev.session_id.as_str()),
              }),
            );
            workers.insert(ev.session_id, worker.clone());
            emitter.emit(crate::events::PageEvent::Worker(worker));
          },
          _ => {},
        }
      }
      for worker in workers.into_values() {
        worker.did_close();
      }
    })
    .abort_handle()
  }

  /// `Inspector.targetCrashed` is `page.on('crash')` (Playwright's
  /// `crPage._onTargetCrashed`).
  fn spawn_crash_listener(&self) -> tokio::task::AbortHandle {
    let events = self.events.clone();
    let mut rx = self
      .transport
      .tap_event_methods(&["Inspector.targetCrashed"], self.session_id.as_deref());
    tokio::spawn(async move {
      while rx.recv().await.is_some() {
        events.emit(crate::events::PageEvent::Crash);
      }
    })
    .abort_handle()
  }

  fn spawn_target_gone_listener(&self) -> tokio::task::AbortHandle {
    let page = self.clone();
    let mut rx = self
      .transport
      .tap_event_methods(&["Target.targetDestroyed", "Target.detachedFromTarget"], None);
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        let Some(params) = event.get("params") else {
          continue;
        };
        let method = event.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let gone = match ferridriver_cdp_proto::CdpEvent::parse(method, params) {
          Some(ferridriver_cdp_proto::CdpEvent::TargetDestroyed(ev)) => ev.target_id == *page.target_id,
          Some(ferridriver_cdp_proto::CdpEvent::DetachedFromTarget(ev)) => {
            page.session_id.as_deref() == Some(ev.session_id.as_str())
          },
          _ => false,
        };
        if gone {
          page.dispose_local();
          return;
        }
      }
    })
    .abort_handle()
  }

  fn spawn_web_error_listener(
    transport: &Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    emitter: crate::events::EventEmitter,
    page_backref: crate::backend::PageBackref,
  ) -> tokio::task::AbortHandle {
    // Tapped here, not in the task: the tap delivers only what arrives
    // after it exists, and a task is not polled the moment it is
    // spawned. An event in between would never be seen.
    let mut rx = transport.tap_event_methods(&["Runtime.exceptionThrown"], session_id.as_deref());
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        let Some(exception_details) = event.get("params").and_then(|p| p.get("exceptionDetails")) else {
          continue;
        };
        let details = cdp_exception_to_error_details(exception_details);
        let location = cdp_exception_to_location(exception_details);
        let web_err = match page_backref.upgrade() {
          Some(page) => crate::web_error::WebError::new(&page, details, location),
          None => crate::web_error::WebError::new_detached(details, location),
        };
        emitter.emit(crate::events::PageEvent::PageError(web_err));
      }
    })
    .abort_handle()
  }

  /// `Page.frameNavigated`'s `frame.name` is optional, and a browser that
  /// leaves it out still names a child frame in the `name` attribute of
  /// the element that owns it, which is where this reads it from.
  fn spawn_frame_name_listener(
    transport: &Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  ) -> tokio::task::AbortHandle {
    let mut rx = transport.tap_event_methods(&["Page.frameAttached", "Page.frameNavigated"], session_id.as_deref());
    let transport = Arc::clone(transport);
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        // Asked when the frame attaches, so the name is known before its
        // first document finishes loading; asked again when a navigation
        // arrives without it, since the owner's attribute may have changed.
        let frame_id = match event.get("method").and_then(serde_json::Value::as_str) {
          Some("Page.frameAttached") => event.pointer("/params/frameId"),
          _ => event
            .pointer("/params/frame")
            .filter(|frame| frame.get("parentId").is_some() && frame.get("name").is_none())
            .and_then(|frame| frame.get("id")),
        };
        let Some(frame_id) = frame_id.and_then(serde_json::Value::as_str).map(str::to_owned) else {
          continue;
        };
        let (transport, session_id, cache) = (Arc::clone(&transport), session_id.clone(), Arc::clone(&cache));
        tokio::spawn(async move {
          let session = session_id.as_deref();
          let Ok(owner) = transport
            .send(
              session,
              &ferridriver_cdp_proto::dom::GetFrameOwner { frame_id: &frame_id },
            )
            .await
          else {
            return;
          };
          let Ok(described) = transport
            .send(
              session,
              &ferridriver_cdp_proto::dom::DescribeNode {
                backend_node_id: Some(owner.backend_node_id),
                ..Default::default()
              },
            )
            .await
          else {
            return;
          };
          let name = described
            .node
            .attributes
            .as_deref()
            .and_then(|attributes| {
              attributes
                .chunks(2)
                .find(|pair| pair.first().map(String::as_str) == Some("name"))
                .and_then(|pair| pair.get(1).map(String::as_str))
            })
            .filter(|name| !name.is_empty());
          if let Some(name) = name {
            cache
              .lock()
              .unwrap_or_else(std::sync::PoisonError::into_inner)
              .rename(&frame_id, name);
          }
        });
      }
    })
    .abort_handle()
  }

  pub(super) fn spawn_network_listener(
    transport: Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    target_id: Arc<str>,
    network_log: Arc<RwLock<Vec<NetworkRequest>>>,
    emitter: crate::events::EventEmitter,
    nav_request_slot: crate::network::NavRequestSlot,
  ) -> tokio::task::AbortHandle {
    // `Page` rides the same tap so a document's commit is seen in wire
    // order after its request: that is what lets a navigation waiter
    // tell "no request" from "request not processed yet".
    let mut rx = transport.tap_event_domains(&["Network", "Page"], session_id.as_deref());
    let commits = nav_request_slot.clone();
    let main_frame = Arc::clone(&target_id);
    let tracker: Arc<NetworkTracker> = Arc::new(NetworkTracker::new(
      transport,
      session_id.clone(),
      target_id,
      nav_request_slot,
    ));
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = event.get("params");
        match method {
          "Page.frameNavigated" => {
            let frame = params.and_then(|p| p.get("frame"));
            if let Some(loader_id) = frame
              .filter(|f| f.get("id").and_then(|v| v.as_str()) == Some(&*main_frame))
              .and_then(|f| f.get("loaderId"))
              .and_then(|v| v.as_str())
            {
              commits.commit_loader(loader_id.to_string());
            }
          },
          "Network.requestWillBeSent" => {
            if let Some(p) = params {
              tracker.on_request_will_be_sent(p, &network_log, &emitter).await;
            }
          },
          "Network.requestWillBeSentExtraInfo" => {
            if let Some(p) = params {
              tracker.on_request_extra_info(p).await;
            }
          },
          "Network.responseReceived" => {
            if let Some(p) = params {
              tracker.on_response_received(p, &emitter).await;
            }
          },
          "Network.responseReceivedExtraInfo" => {
            if let Some(p) = params {
              tracker.on_response_extra_info(p).await;
            }
          },
          "Network.loadingFinished" => {
            if let Some(p) = params {
              tracker.on_loading_finished(p, &emitter).await;
            }
          },
          "Network.loadingFailed" => {
            if let Some(p) = params {
              tracker.on_loading_failed(p, &emitter).await;
            }
          },
          "Network.webSocketCreated" => {
            if let Some(p) = params {
              tracker.on_websocket_created(p, &emitter).await;
            }
          },
          "Network.webSocketFrameSent" => {
            if let Some(p) = params {
              tracker.on_websocket_frame_sent(p).await;
            }
          },
          "Network.webSocketFrameReceived" => {
            if let Some(p) = params {
              tracker.on_websocket_frame_received(p).await;
            }
          },
          "Network.webSocketFrameError" => {
            if let Some(p) = params {
              tracker.on_websocket_error(p).await;
            }
          },
          "Network.webSocketClosed" => {
            if let Some(p) = params {
              tracker.on_websocket_closed(p).await;
            }
          },
          _ => {},
        }
      }
    })
    .abort_handle()
  }

  pub(super) fn spawn_dialog_listener(
    transport: Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    dialog_log: Arc<RwLock<Vec<crate::state::DialogEvent>>>,
    _emitter: crate::events::EventEmitter,
    dialog_manager: crate::dialog::DialogManager,
    page_backref: crate::backend::PageBackref,
  ) -> tokio::task::AbortHandle {
    // Tapped here, not in the task: the tap delivers only what arrives
    // after it exists, and a task is not polled the moment it is
    // spawned. A dialog that opened in between was never answered, and
    // the page stayed blocked behind it.
    let mut rx = transport.tap_event_methods(
      &["Page.javascriptDialogOpening", "Page.javascriptDialogClosed"],
      session_id.as_deref(),
    );
    tokio::spawn(async move {
      while let Some(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        // Playwright's `crPage._onDialogClosed`: a dialog the browser
        // closed without us still closes.
        if event.get("method").and_then(|v| v.as_str()) == Some("Page.javascriptDialogClosed") {
          dialog_manager.closed_in_browser();
          continue;
        }
        let Some(params) = event.get("params") else {
          continue;
        };
        let Ok(payload) =
          serde_json::from_value::<ferridriver_cdp_proto::page::JavascriptDialogOpeningEvent>(params.clone())
        else {
          continue;
        };
        let dialog_type_str = if payload.dialog_type.is_empty() {
          "alert".to_string()
        } else {
          payload.dialog_type
        };
        let message = payload.message;
        let default_value = payload.default_prompt;
        let dialog_type = crate::dialog::DialogType::parse(&dialog_type_str);

        // Build the responder: a closure that translates the user's
        // accept/dismiss into CDP `Page.handleJavaScriptDialog`. The
        // closure captures an `Arc` of the transport + session id so
        // it stays valid across the dialog's lifetime.
        let responder_transport = Arc::clone(&transport);
        let responder_session = session_id.clone();
        let responder: crate::dialog::DialogResponder = Arc::new(move |response| {
          let transport = Arc::clone(&responder_transport);
          let session = responder_session.clone();
          Box::pin(async move {
            let (accept, prompt_text) = match response {
              crate::dialog::DialogResponse::Accept { prompt_text } => (true, prompt_text),
              crate::dialog::DialogResponse::Dismiss => (false, None),
            };
            transport
              .send(
                session.as_deref(),
                &ferridriver_cdp_proto::page::HandleJavaScriptDialog {
                  accept,
                  prompt_text: prompt_text.as_deref(),
                },
              )
              .await
              .map(|_| ())
          })
        });

        let dialog = crate::dialog::Dialog::new_with_manager(
          dialog_type,
          message.clone(),
          default_value.clone(),
          responder,
          Some(dialog_manager.clone()),
          page_backref.weak(),
        );

        // Synchronous dialog dispatch — mirrors Playwright's
        // `DialogManager.dialogDidOpen`. Each registered handler is
        // called with the live `Dialog` and returns `true` to claim
        // ownership. If no handler claims, the manager auto-closes
        // (accept for `beforeunload`, dismiss otherwise). There is no
        // grace window and no race: `did_open` calls handlers
        // synchronously in the same stack as the CDP event arrival.
        dialog_manager.did_open(dialog);

        crate::state::push_capped(
          &mut *dialog_log.write().await,
          crate::state::DialogEvent {
            dialog_type: dialog_type_str,
            message,
            action: "dispatched".to_string(),
          },
          crate::state::DIALOG_LOG_CAP,
        );
      }
    })
    .abort_handle()
  }

  /// Listen for `Page.fileChooserOpened` and dispatch a live
  /// [`crate::file_chooser::FileChooser`] through the page's
  /// [`crate::file_chooser::FileChooserManager`].
  ///
  /// Enables `Page.setInterceptFileChooserDialog({ enabled: true })`
  /// at listener-spawn time so the native file picker is suppressed —
  /// mirrors Playwright's `_updateFileChooserInterception` flow. We
  /// intercept unconditionally because the synchronous claim path
  /// in [`crate::file_chooser::FileChooserManager::did_open`] makes
  /// toggling interception per-listener-count (the way Playwright does
  /// it) racy with the user's listener registration.
  ///
  /// The event carries `backendNodeId` + `mode:'selectSingle'|'selectMultiple'`.
  /// Resolution to an [`crate::element_handle::ElementHandle`] goes
  /// through `DOM.resolveNode` + [`crate::page::Page`] via the
  /// `page_backref` weak — the listener task runs before the outer
  /// `Arc<Page>` exists, so the backref is checked per-event and
  /// silently drops events that arrive before the backref is
  /// populated (matches the Playwright server's "frame/context may go
  /// away" guard).
  fn spawn_file_chooser_listener(
    transport: &Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    file_chooser_manager: crate::file_chooser::FileChooserManager,
    page_backref: crate::backend::PageBackref,
  ) -> tokio::task::AbortHandle {
    // Subscribe FIRST, and before the task exists, so we don't race:
    // routed subscriptions only deliver events published AFTER
    // subscription, a task is not polled the moment it is spawned, and a
    // fast test can trigger the picker before our enable-intercept reply
    // lands. The subscription call is synchronous and cheap.
    let mut rx = transport.tap_event_methods(&["Page.fileChooserOpened"], session_id.as_deref());
    tokio::spawn(async move {
      // We do NOT fire `Page.setInterceptFileChooserDialog` here.
      // Mirrors Playwright's `_updateFileChooserInterception` lazy
      // pattern (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:1009`):
      // interception is enabled only when a `filechooser` listener
      // is registered. The CDP command is fired by
      // `update_file_chooser_intercept` from the page's
      // `on('filechooser', ...)` registration path. Saves one RTT per
      // newly-opened page (~5ms) when no test uses file pickers.
      while let Some(event) = rx.recv().await {
        if let Some(ref expected_sid) = session_id {
          let event_sid = event.get("sessionId").and_then(|v| v.as_str());
          if event_sid != Some(&**expected_sid) {
            continue;
          }
        }
        let Some(params) = event.get("params") else {
          continue;
        };
        let Ok(payload) = serde_json::from_value::<ferridriver_cdp_proto::page::FileChooserOpenedEvent>(params.clone())
        else {
          continue;
        };
        let Some(backend_node_id) = payload.backend_node_id else {
          continue;
        };
        let is_multiple = payload.mode == "selectMultiple";

        // Upgrade the Weak back-reference; the outer Page may not
        // exist yet (listener spawned before `Page::new` populated
        // the backref) or may have been dropped. Either way, no
        // valid target — skip.
        let Some(page) = page_backref.upgrade() else {
          continue;
        };

        // Resolve backendNodeId -> AnyElement -> ElementHandle. This
        // is async, so we spawn the per-event work to keep the
        // outer subscription loop draining — rapid file-picker
        // triggers shouldn't queue up behind a slow DOM resolve.
        let page_clone = Arc::clone(&page);
        let manager_clone = file_chooser_manager.clone();
        tokio::spawn(async move {
          let Ok(element) = page_clone
            .inner()
            .resolve_backend_node(backend_node_id, "filechooser")
            .await
          else {
            // Frame/context may have gone away mid-resolve; Playwright
            // has the same guard and silently returns.
            return;
          };
          let Ok(handle) =
            crate::element_handle::ElementHandle::from_any_element(Arc::clone(&page_clone), element).await
          else {
            return;
          };
          let chooser = crate::file_chooser::FileChooser::new(handle, is_multiple);
          manager_clone.did_open(&chooser);
        });
      }
    })
    .abort_handle()
  }

  /// Listen for `Browser.downloadWillBegin` / `Browser.downloadProgress`
  /// CDP events on this page and dispatch them through the page's
  /// [`crate::download::DownloadManager`].
  ///
  /// Sequence matters: subscribe to the event stream **before**
  /// sending `Browser.setDownloadBehavior`. A fast `<a download>` click
  /// can trigger `Browser.downloadWillBegin` before the configuration
  /// reply lands; subscribing after would race the event. Same
  /// rationale as the file-chooser listener's
  /// `setInterceptFileChooserDialog` ordering.
  ///
  /// Behaviour: `allowAndName` so Chrome writes each download to
  /// `<downloadPath>/<guid>` instead of the server's suggested name
  /// (protects tests against filename collisions on parallel downloads
  /// sharing a tempdir and matches Playwright's own behaviour at
  /// `server/chromium/crBrowser.ts:354`). `eventsEnabled: true` so
  /// `downloadProgress` fires terminal-state events — without it we'd
  /// never resolve `path()`.
  ///
  /// Filtering: `Browser.downloadWillBegin` carries `frameId`. We
  /// claim events whose `frameId` matches our page's main frame id
  /// (populated by the navigation path) so a multi-page browser doesn't
  /// cross-wire downloads. When a download fires before we've cached a
  /// main-frame id, we accept it optimistically — matches Playwright's
  /// `_findOwningPage` behaviour, which also falls through when it
  /// cannot resolve the frame.
  fn spawn_download_listener(
    transport: Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    browser_context_id: Option<Arc<str>>,
    download_manager: crate::download::DownloadManager,
    downloads_dir: Arc<DownloadsDir>,
    page_backref: crate::backend::PageBackref,
  ) -> tokio::task::AbortHandle {
    // Subscribe FIRST, and before the task exists, to avoid racing any
    // future enable reply: a task is not polled the moment it is spawned.
    let mut rx = transport.tap_event_domains(&["Browser"], session_id.as_deref());
    tokio::spawn(async move {
      // We do NOT fire `Browser.setDownloadBehavior` here. Mirrors
      // Playwright's lazy pattern: download behaviour is configured
      // when a `download` listener registers (`page.on('download', ...)`,
      // `wait_for_download`) — see `enable_download_behavior` below.
      // Saves one RTT per page when no test uses downloads. Note:
      // `apply_context_options` still fires `setDownloadBehavior` when
      // the `BrowserContextOptions.accept_downloads` field is
      // explicitly set, so opt-in callers keep working.
      let _ = browser_context_id;
      let _ = downloads_dir;
      while let Some(event) = rx.recv().await {
        // `Browser.downloadWillBegin` / `Browser.downloadProgress` fire
        // on the root browser session (no `sessionId`) when
        // `eventsEnabled: true`. Events with a `sessionId` come from
        // page targets; those are irrelevant to downloads. Accept
        // events whose `sessionId` is absent OR matches ours — CDP's
        // flatten-mode behaviour varies by Chrome version and we want
        // to cover both.
        let event_sid = event.get("sessionId").and_then(|v| v.as_str());
        if let (Some(expected), Some(got)) = (session_id.as_deref(), event_sid)
          && got != expected
        {
          continue;
        }
        let Some(params) = event.get("params") else {
          continue;
        };
        match event.get("method").and_then(|m| m.as_str()) {
          Some("Browser.downloadWillBegin") => Self::on_download_will_begin(
            params,
            &transport,
            session_id.as_ref(),
            browser_context_id.as_ref(),
            &download_manager,
            &downloads_dir,
            &page_backref,
          ),
          Some("Browser.downloadProgress") => Self::on_download_progress(params, &download_manager, &downloads_dir),
          _ => {},
        }
      }
    })
    .abort_handle()
  }

  fn on_download_will_begin(
    params: &serde_json::Value,
    transport: &Arc<CdpTransport>,
    session_id: Option<&Arc<str>>,
    browser_context_id: Option<&Arc<str>>,
    download_manager: &crate::download::DownloadManager,
    downloads_dir: &DownloadsDir,
    page_backref: &crate::backend::PageBackref,
  ) {
    let Ok(payload) = serde_json::from_value::<ferridriver_cdp_proto::browser::DownloadWillBeginEvent>(params.clone())
    else {
      return;
    };
    if payload.guid.is_empty() {
      return;
    }
    let guid = payload.guid;
    let url = payload.url;
    let suggested = payload.suggested_filename;

    let Some(page) = page_backref.upgrade() else {
      return;
    };

    // Build the canceler that issues `Browser.cancelDownload`.
    let transport_c = Arc::clone(transport);
    let session_c = session_id.cloned();
    let ctx_c = browser_context_id.cloned();
    let guid_for_cancel = guid.clone();
    let canceler: crate::download::DownloadCanceler = Arc::new(move || {
      let transport = Arc::clone(&transport_c);
      let session = session_c.clone();
      let ctx = ctx_c.clone();
      let guid = guid_for_cancel.clone();
      Box::pin(async move {
        transport
          .send(
            session.as_deref(),
            &ferridriver_cdp_proto::browser::CancelDownload {
              guid: &guid,
              browser_context_id: ctx.as_deref(),
            },
          )
          .await
          .map(|_| ())
          .map_err(|e| crate::error::FerriError::protocol("Browser.cancelDownload", e.to_string()))
      })
    });

    let download = crate::download::Download::new(
      &page,
      guid,
      url,
      suggested,
      downloads_dir.path().to_path_buf(),
      canceler,
    );
    download_manager.did_open(&download);
  }

  fn on_download_progress(
    params: &serde_json::Value,
    download_manager: &crate::download::DownloadManager,
    downloads_dir: &DownloadsDir,
  ) {
    let Ok(payload) = serde_json::from_value::<ferridriver_cdp_proto::browser::DownloadProgressEvent>(params.clone())
    else {
      return;
    };
    if payload.guid.is_empty() {
      return;
    }
    let guid = payload.guid;
    match payload.state.as_str() {
      "completed" => {
        if let Some(d) = download_manager.take_for_guid(&guid) {
          match downloads_dir.remote.get() {
            None => d.report_finished(None, None),
            // The file is on the browser's machine. It is brought
            // here before the download counts as finished, off
            // this task so later events are not held up.
            Some(remote) => {
              let fetched = (remote.fetch)(guid.clone(), downloads_dir.path().join(&guid));
              tokio::spawn(async move {
                let error = fetched.await.err().map(|error| error.to_string());
                d.report_finished(None, error);
              });
            },
          }
        }
      },
      "canceled" => {
        if let Some(d) = download_manager.take_for_guid(&guid) {
          download_manager.report_canceled(&d, "canceled");
        }
      },
      // "inProgress" — no-op; we only surface terminal state.
      _ => {},
    }
  }

  pub(super) fn context_observer(&self) -> transport::FrameStateObserver {
    let (binding_tx, mut binding_rx) = tokio::sync::mpsc::unbounded_channel::<BindingCall>();
    let transport = Arc::downgrade(&self.transport);
    let exposed_fns = Arc::clone(&self.exposed_fns);
    let session_id = self.session_id.clone();
    let worker = tokio::spawn(async move {
      while let Some(call) = binding_rx.recv().await {
        let Some(transport) = transport.upgrade() else { break };
        Self::run_binding_call(call, &exposed_fns, &transport, session_id.as_ref()).await;
      }
    });
    self
      .listener_tasks
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(worker.abort_handle());
    let contexts = Arc::clone(&self.frame_contexts);
    let utility = Arc::clone(&self.utility_contexts);
    let notify = Arc::clone(&self.frame_contexts_notify);
    let target = Arc::clone(&self.target_id);
    Arc::new(move |raw, method| {
      if matches!(
        method,
        "Runtime.executionContextCreated"
          | "Runtime.executionContextDestroyed"
          | "Runtime.executionContextsCleared"
          | "Runtime.bindingCalled"
          | "Page.frameDetached"
      ) && let Ok(event) = serde_json::from_slice(raw)
      {
        Self::handle_tracker_event(&event, &contexts, &utility, &notify, &target, &binding_tx);
      }
    })
  }

  pub(super) fn handle_tracker_event(
    event: &serde_json::Value,
    frame_contexts: &Arc<std::sync::RwLock<FxHashMap<String, i64>>>,
    utility_contexts: &Arc<std::sync::RwLock<FxHashMap<String, i64>>>,
    contexts_notify: &Arc<tokio::sync::Notify>,
    target_id: &Arc<str>,
    binding_tx: &tokio::sync::mpsc::UnboundedSender<BindingCall>,
  ) {
    let method = event.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let Some(params) = event.get("params") else {
      return;
    };
    match ferridriver_cdp_proto::CdpEvent::parse(method, params) {
      Some(ferridriver_cdp_proto::CdpEvent::ExecutionContextCreated(ev)) => {
        let ctx_id = ev.context.id;
        if let Some(aux) = ev.context.aux_data {
          let frame_id = aux.frame_id.as_deref().unwrap_or("");
          let is_default = aux.is_default.unwrap_or(false);
          if is_default && !frame_id.is_empty() {
            frame_contexts
              .write()
              .unwrap_or_else(std::sync::PoisonError::into_inner)
              .insert(frame_id.to_string(), ctx_id);
            contexts_notify.notify_waiters();
          } else if !frame_id.is_empty() && ev.context.name == UTILITY_WORLD {
            utility_contexts
              .write()
              .unwrap_or_else(std::sync::PoisonError::into_inner)
              .insert(frame_id.to_string(), ctx_id);
          }
        }
      },
      Some(ferridriver_cdp_proto::CdpEvent::ExecutionContextDestroyed(ev)) => {
        let ctx_id = ev.execution_context_id;
        let mut contexts = frame_contexts
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        contexts.retain(|_, &mut v| v != ctx_id);
        drop(contexts);
        utility_contexts
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .retain(|_, &mut v| v != ctx_id);
      },
      Some(ferridriver_cdp_proto::CdpEvent::ExecutionContextsCleared) => {
        frame_contexts
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .clear();
        utility_contexts
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .clear();
      },
      Some(ferridriver_cdp_proto::CdpEvent::BindingCalled(_)) => {
        let contexts = frame_contexts.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(call) = Self::parse_binding_called(params, &contexts, target_id) {
          let _ = binding_tx.send(call);
        }
      },
      Some(ferridriver_cdp_proto::CdpEvent::FrameDetached(ev)) => {
        frame_contexts
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(&ev.frame_id);
        utility_contexts
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(&ev.frame_id);
      },
      _ => {},
    }
  }
}
