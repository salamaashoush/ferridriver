//! `CdpPage` init scripts, exposed bindings, recording, lifecycle, Fetch interception, and handle disposal.

use super::*;

impl CdpPage {
  // ---- Init Scripts ----

  /// # Errors
  ///
  /// Returns an error if `Page.addScriptToEvaluateOnNewDocument` fails on any of the page's renderers.
  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    self.install_init_script(source).await
  }

  /// # Errors
  ///
  /// Returns an error if `Page.removeScriptToEvaluateOnNewDocument` fails on any of the page's renderers.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    self.uninstall_init_script(identifier).await
  }

  // ---- Exposed Functions ----

  /// The JS source for the binding controller injected as an init script.
  pub const BINDING_CONTROLLER_JS: &'static str = crate::backend::common::BINDING_CONTROLLER_JS;

  /// Arm the page-side binding channel (`Runtime.addBinding` + the
  /// `__fd_bc` controller). The synchronous context observer associates
  /// each binding call with its frame before queuing the callback, so later
  /// context changes cannot reattribute an earlier call.
  async fn ensure_binding_channel(&self) -> Result<()> {
    // Hold the lock across the whole bootstrap so a concurrent caller
    // blocks here until `__fd_bc` is actually defined, instead of racing
    // ahead to `__fd_bc.add(...)` against an undefined controller.
    let mut initialized = self.binding_initialized.lock().await;
    if *initialized {
      return Ok(());
    }
    self
      .cmd("Runtime.addBinding", serde_json::json!({"name": "__fd_binding__"}))
      .await?;
    for renderer in &self.renderer_pages() {
      renderer
        .cmd("Runtime.addBinding", serde_json::json!({"name":"__fd_binding__"}))
        .await?;
    }
    self.add_init_script(Self::BINDING_CONTROLLER_JS).await?;
    self.evaluate_in_renderer_frames(Self::BINDING_CONTROLLER_JS).await?;
    *initialized = true;
    Ok(())
  }

  /// Parse one `Runtime.bindingCalled` into a dispatchable call. Frame
  /// resolution happens here, inline in the wire observer, so it reads a
  /// `frame_contexts` map that is exactly as up to date as the wire —
  /// an iframe's first binding call must not resolve against a map that
  /// has not seen its `executionContextCreated` yet.
  pub(super) fn parse_binding_called(
    params: &serde_json::Value,
    frame_contexts: &FxHashMap<String, i64>,
    main_frame_id: &Arc<str>,
  ) -> Option<BindingCall> {
    if params.get("name").and_then(|v| v.as_str()) != Some("__fd_binding__") {
      return None;
    }
    let payload_str = params.get("payload").and_then(|v| v.as_str()).unwrap_or("{}");
    let payload: serde_json::Value = serde_json::from_str(payload_str).unwrap_or_default();
    let fn_name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let seq = payload.get("seq").and_then(serde_json::Value::as_u64).unwrap_or(0);
    let args: Vec<serde_json::Value> = payload
      .get("args")
      .and_then(|v| v.as_array())
      .cloned()
      .unwrap_or_default();

    // Resolve the calling frame from the event's execution context:
    // iframe callers arrive on their own (default) context. Unmapped
    // contexts (isolated worlds, workers) fall back to the main frame.
    let ctx_id = params.get("executionContextId").and_then(serde_json::Value::as_i64);
    let source_frame = ctx_id
      .and_then(|id| {
        frame_contexts
          .iter()
          .find_map(|(fid, &cid)| (cid == id).then(|| fid.clone()))
      })
      .unwrap_or_else(|| main_frame_id.to_string());
    Some(BindingCall {
      fn_name,
      seq,
      args,
      ctx_id,
      source: crate::events::BindingSource {
        context: String::new(),
        page: main_frame_id.to_string(),
        frame: source_frame,
        live: None,
      },
    })
  }

  /// Invoke one exposed binding and hand its result back to the page.
  ///
  /// Called from the serial dispatch task, and it drives the callback up
  /// to its FIRST SUSPENSION there before handing the rest off. That is
  /// the whole trick: whatever a callback does synchronously (the common
  /// case — `page.exposeFunction('log', msg => log.push(msg))` enqueues
  /// its VM job on first poll) happens in wire order, while a callback
  /// that awaits does not block the calls behind it. Awaiting each
  /// callback to completion instead deadlocks the WebSocket router,
  /// whose handler evaluates into the page and thereby provokes the NEXT
  /// binding call before it returns.
  pub(super) async fn run_binding_call(
    call: BindingCall,
    fns: &Arc<tokio::sync::RwLock<FxHashMap<String, crate::backend::PageBinding>>>,
    transport: &Arc<CdpTransport>,
    session_id: Option<&Arc<str>>,
  ) {
    let BindingCall {
      fn_name,
      seq,
      args,
      ctx_id,
      source,
    } = call;
    // Resolve in the CALLING context — each frame has its own
    // `__fd_bc` controller; delivering to the main frame's default
    // context would leave an iframe caller's promise pending forever.
    let deliver = {
      let transport = Arc::clone(transport);
      let sid = session_id.cloned();
      move |deliver_js: String| {
        let mut eval_params = serde_json::json!({"expression": deliver_js});
        if let Some(id) = ctx_id {
          eval_params["contextId"] = serde_json::json!(id);
        }
        tokio::spawn(async move {
          let _ = transport
            .send_command(sid.as_deref(), "Runtime.evaluate", &eval_params)
            .await;
        });
      }
    };
    let Some(callback) = fns.read().await.get(&fn_name).cloned() else {
      deliver(crate::backend::binding_settle_js(
        seq,
        &Err(format!("Function not found: {fn_name}")),
      ));
      return;
    };
    let resolve_js = move |result: &std::result::Result<String, String>| crate::backend::binding_settle_js(seq, result);
    let mut fut = Box::pin(callback(source, args));
    match futures::poll!(fut.as_mut()) {
      std::task::Poll::Ready(result) => deliver(resolve_js(&result)),
      std::task::Poll::Pending => {
        tokio::spawn(async move {
          let result = fut.await;
          deliver(resolve_js(&result));
        });
      },
    }
  }

  /// # Errors
  ///
  /// Returns an error if the binding channel cannot be set up, or registering the binding as an init script
  /// or in the current frames fails.
  pub async fn expose_binding(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::backend::js_escape(name));
    self.add_init_script(&register_js).await?;
    self.evaluate_in_renderer_frames(&register_js).await?;
    Ok(())
  }

  /// [`Self::expose_binding`] for a page whose FIRST document has not
  /// run yet (a popup parked on `waitForDebuggerOnStart`). Only the
  /// paused-safe half is sent — `Runtime.addBinding` plus the
  /// controller/registration init scripts, which the first document
  /// executes at document-start. The live-document `Runtime.evaluate`
  /// installs are skipped: they would block until resume and deadlock
  /// the popup pump. Mirrors Playwright's `CRPage` binding install,
  /// which is init-script-only for exactly this reason.
  pub(crate) async fn expose_binding_pre_doc(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    {
      let mut initialized = self.binding_initialized.lock().await;
      if !*initialized {
        self
          .configure("Runtime.addBinding", serde_json::json!({"name": "__fd_binding__"}))
          .await?;
        self.add_init_script(Self::BINDING_CONTROLLER_JS).await?;
        // The first document runs the controller init script, so
        // later `expose_binding` calls (post-resume) can evaluate
        // `__fd_bc.add` into it directly.
        *initialized = true;
      }
    }
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    let register_js = format!("globalThis.__fd_bc.add('{}')", crate::backend::js_escape(name));
    self.add_init_script(&register_js).await?;
    // Best-effort live-document install, spawned so a still-paused
    // target cannot stall the pump: on the pause-held path the eval
    // completes right after resume; on the watchdog-resumed path it
    // covers the already-running document (the controller init script
    // guards double-install).
    let this = self.clone();
    let controller_and_register = format!("{};{register_js}", Self::BINDING_CONTROLLER_JS);
    tokio::spawn(async move {
      let _ = this.evaluate(&controller_and_register).await;
    });
    Ok(())
  }

  /// Register a binding the page reaches only through
  /// `__playwright__binding__controller__.callBinding`, never as a global:
  /// the callback behind a function passed as an argument.
  ///
  /// # Errors
  ///
  /// Returns an error if the binding channel cannot be set up.
  pub async fn expose_callback(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.ensure_binding_channel().await?;
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    Ok(())
  }

  /// [`Self::expose_callback`] for a page whose first document has not run
  /// yet: the controller arrives with that document's init scripts.
  pub(crate) async fn expose_callback_pre_doc(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    {
      let mut initialized = self.binding_initialized.lock().await;
      if !*initialized {
        self
          .configure("Runtime.addBinding", serde_json::json!({"name": "__fd_binding__"}))
          .await?;
        self.add_init_script(Self::BINDING_CONTROLLER_JS).await?;
        *initialized = true;
      }
    }
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    Ok(())
  }

  pub async fn remove_callback(&self, name: &str) {
    self.exposed_fns.write().await.remove(name);
  }

  /// # Errors
  ///
  /// Returns an error if removing the binding from the current frames fails.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.exposed_fns.write().await.remove(name);
    let js = format!(
      "if(globalThis.__fd_bc)globalThis.__fd_bc.del('{}')",
      crate::backend::js_escape(name)
    );
    self.evaluate_in_renderer_frames(&js).await?;
    Ok(())
  }

  // ---- Native recording ----

  /// Starts Chromium's own recorder on this page; `None` sends the caller
  /// to the screencast instead.
  pub(crate) async fn start_screen_recording(
    &self,
    params: recording::RecordingParams,
  ) -> Option<tokio::sync::watch::Receiver<Option<recording::Recorded>>> {
    if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
      return None;
    }
    self
      .screen_recorder
      .start(&self.transport, &self.target_id, params)
      .await
  }

  /// Stops this page's native recording and hands over its MP4. Called
  /// before anything closes the page's target, which takes the recording
  /// with it.
  pub(crate) async fn finish_screen_recording(&self) {
    self.screen_recorder.finish(&self.transport).await;
  }

  // ---- Lifecycle ----

  /// # Errors
  ///
  /// Never fails: a rejected `Page.close` or `Target.closeTarget` is ignored, since the page is being torn
  /// down either way, and the `Result` keeps the signature every backend shares.
  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<()> {
    if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      self.dispose_local();
      return Ok(());
    }
    self.finish_screen_recording().await;
    // Two CDP paths, matching Playwright's crPage.ts:
    // * runBeforeUnload=true  → `Page.close` — fires beforeunload handlers.
    // * runBeforeUnload=false → `Target.closeTarget` — force-close.
    //
    // Context disposal is still handled by context.close() →
    // BrowserState::remove_context() → Target.disposeBrowserContext (one CDP
    // call kills the context + all its pages, matching Playwright's doClose).
    if opts.run_before_unload.unwrap_or(false) {
      let _ = self
        .transport
        .send_command(self.session_id.as_deref(), "Page.close", &super::super::EMPTY_PARAMS)
        .await;
    } else {
      let _ = self
        .transport
        .send_command(
          None,
          "Target.closeTarget",
          &serde_json::json!({"targetId": &*self.target_id}),
        )
        .await;
    }
    self.events.emit(crate::events::PageEvent::Close);
    // The per-page listeners subscribe to transport-wide event
    // channels that outlive this page — abort them so a closed page
    // stops waking on every future CDP event and releases the state
    // its tasks pin (emitter senders, managers, logs). The `Close`
    // emitted above already sits in the broadcast ring, so page-event
    // subscribers (frame cache, page→context bridge) still observe it.
    self.dispose_local();
    Ok(())
  }

  /// Release everything this page holds on OUR side, without telling the
  /// browser anything.
  ///
  /// Used when the browser is already tearing the page down for us —
  /// `Target.disposeBrowserContext` kills every page in the context in one
  /// call, so sending `Target.closeTarget` per page would just add a
  /// round-trip per page to every context close. What that CDP call does
  /// NOT do is stop our own listeners: each page parks ~10 tasks on
  /// transport-wide taps, and every one holds an `Arc` clone of the
  /// transport. Left running they wake on every future CDP event for the
  /// life of the process, pin the page's emitter/logs/managers, and keep
  /// the transport (and its connection fd) alive after the browser is
  /// gone.
  pub fn dispose_local(&self) {
    if !self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      self.events.emit(crate::events::PageEvent::Close);
    }
    if let Ok(mut guard) = self.listener_tasks.lock() {
      for handle in guard.drain(..) {
        handle.abort();
      }
    }
    self.dispose_renderers();
    {
      let mut cache = self
        .frame_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let main = cache.main_frame_id();
      cache.detach_descendants(&main);
      cache.detach(&main);
    }
    if let Some(sid) = self.session_id.as_deref() {
      self.transport.unregister_session(sid);
    }
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(std::sync::atomic::Ordering::SeqCst)
  }

  // ---- Network Interception ----

  pub(super) async fn ensure_fetch_enabled(&self) -> Result<()> {
    self.ensure_fetch_enabled_current().await?;
    for renderer in &self.renderer_pages() {
      renderer.ensure_fetch_enabled_current().await?;
    }
    Ok(())
  }

  /// Build native CDP `Fetch.RequestPattern`s from the active routes. When
  /// every route is a glob and no auth credentials require intercepting all
  /// challenges, Chrome filters at the network stack and never pauses
  /// non-matching assets (images/fonts/CSS/scripts). Falls back to `"*"`
  /// whenever any route is `Any`, `Regex`, or `Predicate`, or auth handling
  /// is enabled.
  fn fetch_enable_params(routes: &[crate::route::RegisteredRoute], has_creds: bool) -> serde_json::Value {
    let wildcard = || {
      serde_json::json!({
        "patterns": [{"urlPattern": "*", "requestStage": "Request"}],
        "handleAuthRequests": has_creds,
      })
    };
    if has_creds || routes.is_empty() {
      return wildcard();
    }
    let mut patterns: Vec<serde_json::Value> = Vec::with_capacity(routes.len());
    let mut seen = rustc_hash::FxHashSet::default();
    for route in routes {
      let crate::url_matcher::UrlMatcher::Glob { pattern, .. } = &route.matcher else {
        return wildcard();
      };
      let Some(cdp_pat) = glob_to_cdp_url_pattern(pattern) else {
        return wildcard();
      };
      if seen.insert(cdp_pat.clone()) {
        patterns.push(serde_json::json!({"urlPattern": cdp_pat, "requestStage": "Request"}));
      }
    }
    if patterns.is_empty() {
      return wildcard();
    }
    serde_json::json!({
      "patterns": patterns,
      "handleAuthRequests": false,
    })
  }

  pub(super) async fn ensure_fetch_enabled_current(&self) -> Result<()> {
    let _transition = self.fetch.transition.lock().await;
    let has_creds = self.http_credentials.read().await.is_some() || self.proxy_credentials.read().await.is_some();
    let enable = {
      let routes = self.routes.read().await;
      Self::fetch_enable_params(&routes, has_creds)
    };
    if self.fetch.enabled.load(std::sync::atomic::Ordering::SeqCst) {
      let unchanged = self
        .fetch
        .last_enable
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        == Some(&enable);
      if unchanged {
        return Ok(());
      }
      // Patterns or auth handling changed: re-enable with the updated pattern set.
      self.cmd("Fetch.enable", enable.clone()).await?;
      *self
        .fetch
        .last_enable
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(enable);
      return Ok(());
    }

    let handle = Self::spawn_fetch_listener(
      Arc::clone(&self.transport),
      self.session_id.clone(),
      Arc::clone(&self.routes),
      Credentials {
        server: Arc::clone(&self.http_credentials),
        proxy: Arc::clone(&self.proxy_credentials),
      },
    );
    // Dedicated slot so the disable path can abort exactly this loop;
    // also on `listener_tasks` for page-close cleanup (abort is
    // idempotent). A stale handle in the slot is already-aborted.
    if let Ok(mut slot) = self.fetch.interceptor.lock() {
      *slot = Some(handle.clone());
    }
    if let Ok(mut guard) = self.listener_tasks.lock() {
      guard.push(handle.clone());
    }
    // A request the renderer answers from its memory cache never reaches
    // the network, so no route would see it.
    let _ = self
      .configure("Network.setCacheDisabled", serde_json::json!({ "cacheDisabled": true }))
      .await;
    if self.setup.is_paused() {
      // A popup in a browsing instance of its own (`noopener`) answers
      // nothing until it runs. Waiting here for the answer resumed it, and
      // its first request left before interception was on.
      self.fetch.enabled.store(true, std::sync::atomic::Ordering::SeqCst);
      *self
        .fetch
        .last_enable
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(enable.clone());
      let fetch = Arc::clone(&self.fetch);
      let transport = Arc::clone(&self.transport);
      let session = self.session_id.clone();
      return self
        .setup
        .write(Box::pin(async move {
          let answer = transport
            .send_command(session.as_deref(), "Fetch.enable", &enable)
            .await;
          if answer.is_err() {
            fetch.enabled.store(false, std::sync::atomic::Ordering::SeqCst);
            *fetch
              .last_enable
              .lock()
              .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            handle.abort();
          }
          answer.map(|_| ())
        }))
        .await;
    }
    if let Err(error) = self.cmd("Fetch.enable", enable.clone()).await {
      self.fetch.enabled.store(false, std::sync::atomic::Ordering::SeqCst);
      handle.abort();
      return Err(error);
    }

    *self
      .fetch
      .last_enable
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(enable);
    self.fetch.enabled.store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(())
  }

  pub(super) fn spawn_fetch_listener(
    transport: Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
    credentials: Credentials,
  ) -> tokio::task::AbortHandle {
    // Subscribe before enabling Fetch: Chrome can pause a request before its enable reply.
    let rx = transport.tap_event_domains(&["Fetch"], session_id.as_deref());
    tokio::spawn(async move {
      Self::handle_fetch_events(transport, session_id, routes, credentials, rx).await;
    })
    .abort_handle()
  }

  /// `Fetch.disable` + tear down the interceptor loop. Both `unroute`
  /// paths call this when the last route goes away; leaving the loop
  /// alive would double-process `Fetch.requestPaused` after the next
  /// `route` call re-enables interception (see `fetch_interceptor`).
  async fn disable_fetch_interception(&self) {
    self.disable_fetch_interception_current().await;
    for renderer in &self.renderer_pages() {
      renderer.disable_fetch_interception_current().await;
    }
  }

  async fn disable_fetch_interception_current(&self) {
    let _transition = self.fetch.transition.lock().await;
    self.fetch.enabled.store(false, std::sync::atomic::Ordering::SeqCst);
    *self
      .fetch
      .last_enable
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    let _ = self.cmd("Fetch.disable", serde_json::json!({})).await;
    let _ = self
      .cmd(
        "Network.setCacheDisabled",
        serde_json::json!({ "cacheDisabled": false }),
      )
      .await;
    if let Ok(mut slot) = self.fetch.interceptor.lock()
      && let Some(handle) = slot.take()
    {
      handle.abort();
    }
  }

  async fn handle_fetch_events(
    transport: Arc<CdpTransport>,
    session_id: Option<Arc<str>>,
    routes: Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
    credentials: Credentials,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>>,
  ) {
    let mut attempted_auth = rustc_hash::FxHashSet::<String>::default();
    while let Some(event) = rx.recv().await {
      if let Some(ref expected_sid) = session_id {
        let event_sid = event.get("sessionId").and_then(|v| v.as_str());
        if event_sid != Some(&**expected_sid) {
          continue;
        }
      }
      let Some(params) = event.get("params") else { continue };
      match event.get("method").and_then(|m| m.as_str()) {
        Some("Fetch.authRequired") => {
          let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
          let challenge = Self::auth_challenge_response(params, &credentials, &mut attempted_auth).await;
          let response = serde_json::json!({ "requestId": request_id, "authChallengeResponse": challenge });
          // Spawned so the auth round-trip never stalls the loop —
          // while this listener is blocked the broadcast buffer fills,
          // and a lapped `Fetch.requestPaused` is a request Chrome
          // holds paused forever.
          let transport2 = Arc::clone(&transport);
          let sid2 = session_id.clone();
          tokio::spawn(async move {
            let _ = transport2
              .send_command(sid2.as_deref(), "Fetch.continueWithAuth", &response)
              .await;
          });
        },
        Some("Fetch.requestPaused") => {
          Self::on_request_paused(params, &transport, session_id.as_ref(), &routes).await;
        },
        _ => {},
      }
    }
  }

  /// The `authChallengeResponse` for a `Fetch.authRequired`.
  ///
  /// Playwright answers with the first entry bound to no origin or to
  /// the request's (`crNetworkManager.ts::_onAuthRequired`). When none
  /// matches, the browser's default stands and the 401 surfaces, rather
  /// than credentials going to a host they were not meant for. A
  /// challenge from the proxy is answered only with the proxy's own
  /// credentials, never the website's, and each source gets one attempt
  /// per request.
  async fn auth_challenge_response(
    params: &serde_json::Value,
    credentials: &Credentials,
    attempted_auth: &mut rustc_hash::FxHashSet<String>,
  ) -> serde_json::Value {
    let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
    let req_url = params
      .get("request")
      .and_then(|r| r.get("url"))
      .and_then(|v| v.as_str())
      .unwrap_or("");
    let from_proxy = params
      .get("authChallenge")
      .and_then(|c| c.get("source"))
      .and_then(|v| v.as_str())
      == Some("Proxy");
    let attempt = format!("{request_id}:{}", if from_proxy { "proxy" } else { "server" });
    if from_proxy {
      let proxy = credentials.proxy.read().await.clone();
      return match proxy {
        Some(_) if !attempted_auth.insert(attempt) => serde_json::json!({ "response": "CancelAuth" }),
        Some(entry) => serde_json::json!({
          "response": "ProvideCredentials",
          "username": entry.username,
          "password": entry.password,
        }),
        None => serde_json::json!({ "response": "Default" }),
      };
    }
    let creds = credentials.server.read().await;
    let Some(list) = creds.as_deref() else {
      return serde_json::json!({ "response": "CancelAuth" });
    };
    match crate::options::matching_http_credentials(list, req_url) {
      // A second challenge for one request means the credentials
      // were refused; answering again would never end.
      Some(_) if !attempted_auth.insert(attempt) => serde_json::json!({ "response": "CancelAuth" }),
      Some(entry) => serde_json::json!({
        "response": "ProvideCredentials",
        "username": entry.username,
        "password": entry.password,
      }),
      None => serde_json::json!({ "response": "Default" }),
    }
  }

  async fn on_request_paused(
    params: &serde_json::Value,
    transport: &Arc<CdpTransport>,
    session_id: Option<&Arc<str>>,
    routes: &Arc<tokio::sync::RwLock<Vec<crate::route::RegisteredRoute>>>,
  ) {
    let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("");
    let req_obj = params.get("request");
    // Borrow URL directly from the JSON event — zero allocation for matching.
    let url = req_obj
      .and_then(|r| r.get("url"))
      .and_then(|v| v.as_str())
      .unwrap_or("");

    // Cheap pre-check under a read lock — the full request payload is
    // only parsed when at least one live route matches.
    let has_match = {
      let guard = routes.read().await;
      crate::route::any_matching_route(&guard, url)
    };

    let transport2 = Arc::clone(transport);
    let sid2 = session_id.cloned();
    let request_id_owned = request_id.to_string();
    if has_match {
      let method = req_obj
        .and_then(|r| r.get("method"))
        .and_then(|v| v.as_str())
        .unwrap_or("GET");
      let resource_type = params.get("resourceType").and_then(|v| v.as_str()).unwrap_or("");
      let post_data = req_obj.and_then(|r| r.get("postData")).and_then(|v| v.as_str());
      let headers: FxHashMap<String, String> = req_obj
        .and_then(|r| r.get("headers"))
        .and_then(|h| h.as_object())
        .map(|obj| {
          obj
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect()
        })
        .unwrap_or_default();

      let intercepted = crate::route::InterceptedRequest {
        request_id: request_id.to_string(),
        url: url.to_string(),
        method: method.to_string(),
        headers,
        post_data: post_data.map(str::to_string),
        resource_type: resource_type.to_string(),
      };

      // Each paused request is resolved on its own task. Running the
      // user handler + resolution round-trip inline serialized every
      // intercepted request behind the slowest handler AND stalled
      // this listener — once the broadcast buffer lapped, a dropped
      // `Fetch.requestPaused` was a request that never continued
      // (permanent page hang). `run_route_chain` consumes each
      // handler's `times` budget under the routes write lock, so the
      // budget stays correct across concurrent requests. WebKit's
      // interception listener spawns the same way
      // (`webkit/events.rs::handle_request_intercepted`).
      let routes2 = Arc::clone(routes);
      tokio::spawn(async move {
        let action = crate::route::run_route_chain(&routes2, intercepted).await;
        Self::execute_route_action(&transport2, sid2.as_deref(), &request_id_owned, Some(action)).await;
      });
    } else {
      // No matching route — continue on a spawned task so even the
      // plain-continue round-trip can't back the listener up under
      // a request burst.
      tokio::spawn(async move {
        let _ = transport2
          .send_command(
            sid2.as_deref(),
            "Fetch.continueRequest",
            &serde_json::json!({"requestId": request_id_owned}),
          )
          .await;
      });
    }
  }

  async fn execute_route_action(
    transport: &CdpTransport,
    session_id: Option<&str>,
    request_id: &str,
    action: Option<crate::route::RouteAction>,
  ) {
    match action {
      Some(crate::route::RouteAction::Fulfill(resp)) => {
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(&resp.body);
        let mut hdrs: Vec<serde_json::Value> = resp
          .headers
          .iter()
          .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
          .collect();
        if let Some(ct) = &resp.content_type
          && !hdrs
            .iter()
            .any(|h| h.get("name").and_then(|n| n.as_str()) == Some("content-type"))
        {
          hdrs.push(serde_json::json!({"name": "content-type", "value": ct}));
        }
        let _ = transport
          .send_command(
            session_id,
            "Fetch.fulfillRequest",
            &serde_json::json!({
                "requestId": request_id,
                "responseCode": resp.status,
                "responsePhrase": crate::route::status_text(resp.status),
                "responseHeaders": hdrs,
                "body": body_b64,
            }),
          )
          .await;
      },
      // `Fallback` is consumed inside `run_route_chain`; if one leaks
      // here (chain bypassed), treat it as a continue with its overrides.
      Some(crate::route::RouteAction::Continue(overrides) | crate::route::RouteAction::Fallback(overrides)) => {
        let mut params = serde_json::json!({"requestId": request_id});
        if let Some(url) = &overrides.url {
          params["url"] = serde_json::Value::String(url.clone());
        }
        if let Some(method) = &overrides.method {
          params["method"] = serde_json::Value::String(method.clone());
        }
        if let Some(headers) = &overrides.headers {
          let hdrs: Vec<serde_json::Value> = headers
            .iter()
            .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
            .collect();
          params["headers"] = serde_json::Value::Array(hdrs);
        }
        if let Some(post_data) = &overrides.post_data {
          params["postData"] = serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(post_data));
        }
        let _ = transport
          .send_command(session_id, "Fetch.continueRequest", &params)
          .await;
      },
      Some(crate::route::RouteAction::Abort(reason)) => {
        let error_reason = match reason.to_lowercase().as_str() {
          "aborted" => "Aborted",
          "accessdenied" => "AccessDenied",
          "addressunreachable" => "AddressUnreachable",
          "blockedbyclient" => "BlockedByClient",
          "connectionfailed" => "ConnectionFailed",
          "connectionrefused" => "ConnectionRefused",
          "connectionreset" => "ConnectionReset",
          "internetdisconnected" => "InternetDisconnected",
          "namenotresolved" => "NameNotResolved",
          "timedout" => "TimedOut",
          _ => "Failed",
        };
        let _ = transport
          .send_command(
            session_id,
            "Fetch.failRequest",
            &serde_json::json!({
                "requestId": request_id,
                "errorReason": error_reason,
            }),
          )
          .await;
      },
      // `PredicateRejected` is chain-internal — `run_route_chain`
      // refunds the budget and keeps walking, so it can never surface
      // here; if it ever leaks, fail open like an unhandled route.
      Some(crate::route::RouteAction::PredicateRejected) | None => {
        let _ = transport
          .send_command(
            session_id,
            "Fetch.continueRequest",
            &serde_json::json!({"requestId": request_id}),
          )
          .await;
      },
    }
  }

  /// Shared transport handle + target id for attaching a public raw
  /// [`crate::cdp_session::CdpSession`] to this page's target.
  pub(crate) fn session_parts(&self) -> (Arc<CdpTransport>, &str) {
    (Arc::clone(&self.transport), &self.target_id)
  }

  /// Transport flavour (`CdpPipe` or `CdpWs`) behind this page.
  #[must_use]
  pub(crate) fn backend_kind(&self) -> super::super::BackendKind {
    self.transport.kind()
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Fetch.enable` on this page or one of its renderers, the target has
  /// closed, or the command outlives its time budget.
  pub async fn route(&self, route: crate::route::RegisteredRoute) -> Result<()> {
    self.routes.write().await.push(route);
    self.ensure_fetch_enabled().await
  }

  /// # Errors
  ///
  /// Never fails; the `Result` keeps the signature every backend shares.
  pub async fn unroute(
    &self,
    matcher: &crate::url_matcher::UrlMatcher,
    scope: crate::route::RouteScope,
    handler_id: Option<usize>,
  ) -> Result<()> {
    let now_empty = {
      let mut routes = self.routes.write().await;
      routes.retain(|r| {
        r.scope != scope || !r.matcher.equivalent(matcher) || handler_id.is_some_and(|id| r.handler_id() != id)
      });
      routes.is_empty()
    };
    if now_empty && self.fetch.enabled.load(std::sync::atomic::Ordering::SeqCst) {
      self.disable_fetch_interception().await;
    } else if !now_empty && self.fetch.enabled.load(std::sync::atomic::Ordering::SeqCst) {
      let _ = self.ensure_fetch_enabled().await;
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; the `Result` keeps the signature every backend shares.
  pub async fn unroute_all(
    &self,
    _behavior: crate::options::UnrouteBehavior,
    scope: Option<crate::route::RouteScope>,
  ) -> Result<()> {
    let now_empty = {
      let mut routes = self.routes.write().await;
      match scope {
        Some(scope) => routes.retain(|r| r.scope != scope),
        None => routes.clear(),
      }
      routes.is_empty()
    };
    if now_empty && self.fetch.enabled.load(std::sync::atomic::Ordering::SeqCst) {
      self.disable_fetch_interception().await;
    } else if !now_empty && self.fetch.enabled.load(std::sync::atomic::Ordering::SeqCst) {
      let _ = self.ensure_fetch_enabled().await;
    }
    Ok(())
  }

  // ── Handle lifecycle ──

  /// Release the CDP `RemoteObject` identified by `object_id` via
  /// `Runtime.releaseObject`. Used by `AnyPage::release_handle` when
  /// disposing a `JSHandle` / `ElementHandle` on a CDP backend.
  ///
  /// # Errors
  ///
  /// Returns the CDP transport error if the call fails. Already-released
  /// objects surface a protocol error containing `No object with id`
  /// which callers (typically `JSHandle::dispose`) may choose to treat
  /// as success — the dispose path here forwards the error as-is so
  /// idempotence is handled client-side by the `disposed` flag, not by
  /// swallowing protocol failures.
  pub(crate) async fn release_object_in_session(&self, object_id: &str, session_id: Option<&str>) -> Result<()> {
    self
      .transport
      .send_command(
        session_id,
        "Runtime.releaseObject",
        &serde_json::json!({"objectId":object_id}),
      )
      .await
      .map(|_| ())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.releaseObject`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn release_object(&self, object_id: &str) -> Result<()> {
    self
      .cmd("Runtime.releaseObject", serde_json::json!({"objectId": object_id}))
      .await
      .map(|_| ())
  }
}
