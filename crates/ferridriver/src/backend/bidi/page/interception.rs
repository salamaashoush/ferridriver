//! `BidiPage` file upload, network interception, lifecycle, init scripts, and exposed functions.

use super::{
  Arc, BINDING_CHANNEL, BidiPage, BidiSession, CONTEXT_SCRIPT, FerriError, FxHashMap, NetworkSettings, Ordering,
  Result, RwLock, execute_bidi_route_action, json, parse_bidi_headers,
};

impl BidiPage {
  // ── File upload ─────────────────────────────────────────────────────────

  /// Set the file list on a resolved `<input type=file>` element via
  /// `BiDi` `input.setFiles` (Playwright's `bidiPage.ts::setInputFilePaths`).
  /// The element's owning context is used so iframe-scoped inputs
  /// resolve in their own browsing context.
  ///
  /// # Errors
  ///
  /// Returns an error when the `input.setFiles` command fails, or on a
  /// browser without it, when Classic `WebDriver` cannot send the paths.
  pub async fn set_input_files_element(&self, element: &super::super::BidiElement, paths: &[String]) -> Result<()> {
    let params = json!({
      "context": &*element.context_id,
      "element": {"sharedId": element.shared_id},
      "files": paths
    });
    if self.session.try_command("input.setFiles", params).await?.is_none() {
      // A node's shared id is its Classic element reference.
      let page = self.session.classic_page(&element.context_id)?;
      return crate::backend::webdriver::element::WebDriverElement::new(page, element.shared_id.clone())
        .set_input_files(paths)
        .await;
    }
    Ok(())
  }

  // ── Network Interception ────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when `network.addIntercept` fails or answers without an intercept id, for
  /// this page or, for a context route, for the session. A failed `network.setCacheBehavior` is
  /// ignored.
  pub async fn route(&self, route: crate::route::RegisteredRoute) -> Result<()> {
    if route.scope == crate::route::RouteScope::Context {
      self
        .session
        .scopes
        .add_route(&self.session, &self.user_context, &route)
        .await?;
    }
    // A popup starts with its context's routes already in place.
    if self
      .routes
      .read()
      .await
      .iter()
      .any(|known| known.scope == route.scope && known.handler_id() == route.handler_id())
    {
      return Ok(());
    }
    let needs_intercept = self.intercept_ids.read().await.is_empty();
    if needs_intercept {
      // Register a single intercept for ALL requests on this context (no urlPatterns).
      // BiDi urlPatterns have limited syntax — filtering happens client-side via regex.
      // This matches Puppeteer's approach.
      let result = self
        .cmd(
          "network.addIntercept",
          json!({
            "phases": ["beforeRequestSent"],
            "contexts": [&*self.context_id]
          }),
        )
        .await?;

      let intercept_id = result
        .get("intercept")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FerriError::protocol("network.addIntercept", "missing intercept id"))?
        .to_string();

      self.intercept_ids.write().await.push(intercept_id);
      // A resource served from the page's memory cache is never requested,
      // so no route would see it.
      let _ = self
        .cmd(
          "network.setCacheBehavior",
          json!({ "cacheBehavior": "bypass", "contexts": [&*self.context_id] }),
        )
        .await;
    }

    // Spawn the route-event listener exactly once per page. It reads the
    // shared `routes` map live, so a route/unroute cycle (which drains
    // `intercept_ids` and removes the BiDi intercept) must NOT re-spawn it.
    self.start_route_listener();

    self.routes.write().await.push(route);

    Ok(())
  }

  /// Resolve one request an intercept blocked: through the page's routes
  /// when one matches, straight on otherwise.
  async fn resolve_blocked_request(
    session: &Arc<BidiSession>,
    routes: &Arc<RwLock<Vec<crate::route::RegisteredRoute>>>,
    network: &Arc<std::sync::Mutex<NetworkSettings>>,
    event: &super::super::transport::BidiEvent,
  ) {
    let req_obj = event.params.get("request");
    let request_id = req_obj
      .and_then(|v| v.get("request"))
      .and_then(|v| v.as_str())
      .unwrap_or("");
    let url = req_obj
      .and_then(|v| v.get("url"))
      .and_then(|v| v.as_str())
      .unwrap_or("");

    let has_match = {
      let guard = routes.read().await;
      crate::route::any_matching_route(&guard, url)
    };

    if has_match {
      let method = req_obj
        .and_then(|r| r.get("method"))
        .and_then(|v| v.as_str())
        .unwrap_or("GET");
      let headers: FxHashMap<String, String> = req_obj
        .and_then(|r| r.get("headers"))
        .map(parse_bidi_headers)
        .unwrap_or_default();

      // The original method/headers are forwarded when a `url` override
      // is applied (continue resolves to the new URL's content with the
      // request's own cookies/headers), so keep a copy before the
      // `InterceptedRequest` moves them into the handler's `Route`.
      let orig_method = method.to_string();
      let orig_headers = headers.clone();
      let body_size = req_obj
        .and_then(|r| r.get("bodySize"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
      let resource_type = req_obj
        .map_or("", |r| super::listeners::resource_type_from_bidi(r, &event.params))
        .to_string();

      let mut intercepted = crate::route::InterceptedRequest {
        request_id: request_id.to_string(),
        url: url.to_string(),
        method: method.to_string(),
        headers,
        post_data: None,
        resource_type,
      };

      // Resolve each blocked request on its own task — running the
      // user handler + resolution round-trip inline serialized all
      // intercepted requests behind the slowest handler and, once
      // the broadcast lapped, dropped `beforeRequestSent` events
      // whose requests then hung blocked forever. Same shape as
      // the CDP Fetch listener.
      let session2 = Arc::clone(session);
      let request_id_owned = request_id.to_string();
      let routes2 = Arc::clone(routes);
      let network = Arc::clone(network);
      tokio::spawn(async move {
        if body_size > 0
          && let Some(bytes) = session2.get_request_body(&request_id_owned).await
        {
          intercepted.post_data = Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        let action = crate::route::run_route_chain(&routes2, intercepted).await;
        let network = network.lock().map(|settings| settings.clone()).unwrap_or_default();
        execute_bidi_route_action(
          &session2.transport,
          &request_id_owned,
          action,
          &orig_method,
          &orig_headers,
          &network,
        )
        .await;
      });
    } else {
      let session2 = Arc::clone(session);
      let request_id_owned = request_id.to_string();
      tokio::spawn(async move {
        let _ = session2
          .transport
          .send(&ferridriver_bidi_proto::network::ContinueRequest {
            request: &request_id_owned,
            body: None,
            headers: None,
            method: None,
            url: None,
          })
          .await;
      });
    }
  }

  /// Spawn the `network.beforeRequestSent` route listener exactly once per
  /// page. Guarded by `route_listener_started`, NOT `intercept_ids`: a
  /// `route`/`unroute` cycle drains `intercept_ids`, so guarding the spawn
  /// on that would leak a second listener on the next `route` — both would
  /// call `network.continueRequest` for the same blocked request and all but
  /// the first fail with "no such request", stalling it (the page sees an
  /// `AbortError`). One listener, reading the shared `routes` map live,
  /// transparently handles handlers added/removed over the page's lifetime.
  pub(super) fn start_route_listener(&self) {
    if self
      .route_listener_started
      .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
      return;
    }
    let forwarded = self
      .forwarded_requests
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take();
    let mut tap = forwarded.is_none().then(|| self.session.transport.tap_events());
    let mut forwarded = forwarded;
    let intercept_ids = Arc::clone(&self.intercept_ids);
    let session = Arc::clone(&self.session);
    let routes = Arc::clone(&self.routes);
    let network = Arc::clone(&self.network);

    let route_listener = tokio::spawn(async move {
      loop {
        let event = match (&mut forwarded, &mut tap) {
          (Some(requests), _) => requests.recv().await,
          (None, Some(tap)) => tap.recv().await,
          (None, None) => None,
        };
        let Some(event) = event else { break };
        if event.method != "network.beforeRequestSent" {
          continue;
        }
        let is_blocked = event
          .params
          .get("isBlocked")
          .and_then(serde_json::Value::as_bool)
          .unwrap_or(false);
        if !is_blocked {
          continue;
        }
        // The intercept covers this page's iframes too, whose requests
        // carry their own browsing context; what makes a blocked request
        // this page's to resolve is that this page's intercept blocked it,
        // or that it was forwarded here as this page's.
        let blocked_by_us = forwarded.is_some() || {
          let ours = intercept_ids.read().await;
          event
            .params
            .get("intercepts")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|ids| {
              ids
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|id| ours.iter().any(|o| o == id))
            })
        };
        if !blocked_by_us {
          continue;
        }

        Self::resolve_blocked_request(&session, &routes, &network, &event).await;
      }
    });
    self.track_listener(route_listener.abort_handle());
  }

  /// # Errors
  ///
  /// Never fails; the `Result` matches the other backends' signature. Failures of
  /// `network.removeIntercept` and `network.setCacheBehavior` are ignored.
  pub async fn unroute(
    &self,
    matcher: &crate::url_matcher::UrlMatcher,
    scope: crate::route::RouteScope,
    handler_id: Option<usize>,
  ) -> Result<()> {
    let keep = |r: &crate::route::RegisteredRoute| {
      r.scope != scope || !r.matcher.equivalent(matcher) || handler_id.is_some_and(|id| r.handler_id() != id)
    };
    if scope == crate::route::RouteScope::Context {
      self
        .session
        .scopes
        .retain_routes(&self.session, &self.user_context, keep)
        .await;
    }
    let mut routes = self.routes.write().await;
    routes.retain(keep);

    // If no routes left, remove the intercept entirely
    if routes.is_empty() {
      let mut ids = self.intercept_ids.write().await;
      for id in ids.drain(..) {
        let _ = self.cmd("network.removeIntercept", json!({"intercept": id})).await;
      }
      let _ = self
        .cmd(
          "network.setCacheBehavior",
          json!({ "cacheBehavior": "default", "contexts": [&*self.context_id] }),
        )
        .await;
    }

    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; the `Result` matches the other backends' signature. Failures of
  /// `network.removeIntercept` and `network.setCacheBehavior` are ignored.
  pub async fn unroute_all(
    &self,
    _behavior: crate::options::UnrouteBehavior,
    scope: Option<crate::route::RouteScope>,
  ) -> Result<()> {
    if scope == Some(crate::route::RouteScope::Context) {
      self
        .session
        .scopes
        .retain_routes(&self.session, &self.user_context, |_| false)
        .await;
    }
    let mut routes = self.routes.write().await;
    match scope {
      Some(scope) => routes.retain(|r| r.scope != scope),
      None => routes.clear(),
    }
    if routes.is_empty() {
      let mut ids = self.intercept_ids.write().await;
      for id in ids.drain(..) {
        let _ = self.cmd("network.removeIntercept", json!({"intercept": id})).await;
      }
      let _ = self
        .cmd(
          "network.setCacheBehavior",
          json!({ "cacheBehavior": "default", "contexts": [&*self.context_id] }),
        )
        .await;
    }
    Ok(())
  }

  // ── Lifecycle ───────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the `browsingContext.close` command fails.
  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<()> {
    // BiDi's `browsingContext.close` takes an optional `promptUnload` flag
    // — true fires `beforeunload` handlers before unloading the context.
    // Mirrors Playwright's `bidiPage.ts:_closePage` param naming.
    self
      .cmd(
        "browsingContext.close",
        json!({
          "context": &*self.context_id,
          "promptUnload": opts.run_before_unload.unwrap_or(false),
        }),
      )
      .await?;
    // `swap` dedupes against the main listener's `contextDestroyed`
    // branch, which also emits `Close` when Firefox tears the context
    // down on its own.
    if !self.closed.swap(true, Ordering::SeqCst) {
      self.events.emit(crate::events::PageEvent::Close);
    }
    // The listeners subscribe to the session-wide event channel, which
    // outlives this page — abort them so a closed page stops waking on
    // every future BiDi event. The `Close` emitted above already sits
    // in the page emitter's ring for its subscribers.
    if let Ok(mut guard) = self.listener_tasks.lock() {
      for handle in guard.drain(..) {
        handle.abort();
      }
    }
    Ok(())
  }

  /// Release this page's local resources without a protocol round trip —
  /// see the CDP `dispose_local` for why context teardown needs this.
  pub fn dispose_local(&self) {
    if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
      return;
    }
    self.events.emit(crate::events::PageEvent::Close);
    if let Ok(mut guard) = self.listener_tasks.lock() {
      for handle in guard.drain(..) {
        handle.abort();
      }
    }
  }

  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.closed.load(Ordering::Relaxed)
  }

  // ── Init Scripts ────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when `script.addPreloadScript` fails or its response has no script id.
  pub async fn add_init_script(&self, source: &str) -> Result<String> {
    let wrapped = format!("() => {{ {source} }}");
    let result = self
      .cmd(
        "script.addPreloadScript",
        json!({
          "functionDeclaration": wrapped,
          "contexts": [&*self.context_id]
        }),
      )
      .await?;

    let bidi_id = result
      .get("script")
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("script.addPreloadScript", "missing script id"))?
      .to_string();

    // Generate our own stable identifier
    let our_id = format!("init-{}", self.preload_scripts.read().await.len());
    self.preload_scripts.write().await.insert(our_id.clone(), bidi_id);

    Ok(our_id)
  }

  /// Install an init script its whole context has, once for the user
  /// context: the pages the browser opens in it run the script in their
  /// first document, which a per-page preload registered after the page
  /// appeared would miss. `key` names the script across the context's
  /// pages.
  ///
  /// # Errors
  ///
  /// Returns an error when the user-context `script.addPreloadScript` fails or its response has no
  /// script id.
  pub async fn add_context_init_script(&self, key: u64, source: &str) -> Result<String> {
    self
      .session
      .scopes
      .add_script(&self.session, &self.user_context, key, &format!("() => {{ {source} }}"))
      .await?;
    Ok(format!("{CONTEXT_SCRIPT}{key}"))
  }

  /// # Errors
  ///
  /// Returns `FerriError::InvalidArgument` when `identifier` is neither a context script identifier
  /// nor an init script of this page, and an error when `script.removePreloadScript` fails.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    if let Some(key) = identifier.strip_prefix(CONTEXT_SCRIPT) {
      let key = key
        .parse()
        .map_err(|_| FerriError::invalid_argument("identifier", format!("init script '{identifier}' not found")))?;
      // Every page of the context holds the identifier; the first to
      // give it up removes the one preload behind it.
      if let Some(script) = self.session.scopes.take_script(&self.user_context, key) {
        self
          .cmd("script.removePreloadScript", json!({"script": script}))
          .await?;
      }
      return Ok(());
    }
    let bidi_id = self
      .preload_scripts
      .write()
      .await
      .remove(identifier)
      .ok_or_else(|| FerriError::invalid_argument("identifier", format!("init script '{identifier}' not found")))?;

    self
      .cmd("script.removePreloadScript", json!({"script": bidi_id}))
      .await?;
    Ok(())
  }

  // ── Exposed Functions ───────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the `script.addPreloadScript` that installs the binding fails.
  /// Installing it in the frames already loaded is best effort.
  pub async fn expose_binding(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.expose_binding_in(name, binding, false).await
  }

  /// [`Self::expose_binding`] for a binding its whole context has. Its
  /// preload goes onto the user context, so a page the browser opens in
  /// it has the function in its first document.
  ///
  /// # Errors
  ///
  /// Returns an error when the user-context `script.addPreloadScript` that installs the binding
  /// fails. Installing it in the frames already loaded is best effort.
  pub async fn expose_context_binding(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.expose_binding_in(name, binding, true).await
  }

  async fn expose_binding_in(
    &self,
    name: &str,
    binding: crate::backend::PageBinding,
    context_wide: bool,
  ) -> Result<()> {
    // The shim receives a BiDi channel as its argument and sends
    // `JSON.stringify({name, id, args})` through it — the driver gets a
    // `script.message` event carrying the payload plus the calling
    // frame in `source.context`. Same shape as Playwright's BiDi
    // binding (`bidiBrowser.ts::doExposePlaywrightBinding` +
    // `bidiPage.ts::_onScriptMessage`); the return value is delivered
    // by resolving the parked promise via a `script.callFunction` into
    // the calling context.
    let js = format!(
      r"(channel) => {{
        window['{name}'] = (...args) => {{
          return new Promise((resolve, reject) => {{
            const id = Math.random().toString(36);
            window.__ferri_exposed = window.__ferri_exposed || {{}};
            window.__ferri_exposed[id] = {{ resolve, reject }};
            channel(JSON.stringify({{name: '{name}', id, args}}));
          }});
        }};
      }}"
    );
    let channel_arg = json!([{
      "type": "channel",
      "value": { "channel": BINDING_CHANNEL },
    }]);

    self
      .preload(&format!("binding:{name}"), &js, &channel_arg, context_wide)
      .await?;

    // Also execute it now in every live frame (Playwright does the
    // same for already-live realms, with the same channel argument).
    self.call_in_frames(&js, &channel_arg).await;

    self.exposed_fns.write().await.insert(name.to_string(), binding);
    Ok(())
  }

  /// Register a binding the page reaches only through
  /// `__playwright__binding__controller__.callBinding`, never as a global.
  ///
  /// # Errors
  ///
  /// Returns an error when the `script.addPreloadScript` that installs the binding controller
  /// fails.
  pub async fn expose_callback(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.ensure_binding_controller(false).await?;
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    Ok(())
  }

  /// [`Self::expose_callback`] for a callback its whole context has.
  ///
  /// # Errors
  ///
  /// Returns an error when the user-context `script.addPreloadScript` that installs the binding
  /// controller fails.
  pub async fn expose_context_callback(&self, name: &str, binding: crate::backend::PageBinding) -> Result<()> {
    self.ensure_binding_controller(true).await?;
    self.exposed_fns.write().await.insert(name.to_string(), binding);
    Ok(())
  }

  /// Register a channel preload script: on this page, or once on its
  /// user context when the whole context has it.
  async fn preload(&self, name: &str, function: &str, arguments: &serde_json::Value, context_wide: bool) -> Result<()> {
    if !context_wide {
      self
        .cmd(
          "script.addPreloadScript",
          json!({"functionDeclaration": function, "arguments": arguments, "contexts": [&*self.context_id]}),
        )
        .await?;
      return Ok(());
    }
    let scopes = &self.session.scopes;
    if !scopes.claim_preload(&self.user_context, name) {
      return Ok(());
    }
    let installed = self
      .cmd(
        "script.addPreloadScript",
        json!({"functionDeclaration": function, "arguments": arguments, "userContexts": [&*self.user_context]}),
      )
      .await;
    if installed.is_err() {
      scopes.release_preload(&self.user_context, name);
    }
    installed.map(|_| ())
  }

  pub async fn remove_callback(&self, name: &str) {
    self.exposed_fns.write().await.remove(name);
  }

  /// Install the controller in every document of this page, present and
  /// future. It speaks the same channel payload as an exposed binding, so
  /// the `script.message` dispatcher settles its calls unchanged.
  async fn ensure_binding_controller(&self, context_wide: bool) -> Result<()> {
    let mut installed = self.binding_controller.lock().await;
    if *installed && !context_wide {
      return Ok(());
    }
    let js = format!(
      r"(channel) => {{
        if (globalThis['{controller}']) return;
        Object.defineProperty(globalThis, '{controller}', {{ configurable: true, value: {{
          callBinding(name, ...args) {{
            return new Promise((resolve, reject) => {{
              const id = Math.random().toString(36).slice(2);
              window.__ferri_exposed = window.__ferri_exposed || {{}};
              window.__ferri_exposed[id] = {{ resolve, reject }};
              channel(JSON.stringify({{ name, id, args }}));
            }});
          }},
        }} }});
      }}",
      controller = crate::protocol::BINDINGS_CONTROLLER,
    );
    let channel_arg = json!([{
      "type": "channel",
      "value": { "channel": BINDING_CHANNEL },
    }]);
    self.preload("controller", &js, &channel_arg, context_wide).await?;
    if *installed {
      return Ok(());
    }
    self.call_in_frames(&js, &channel_arg).await;
    *installed = true;
    Ok(())
  }

  /// # Errors
  ///
  /// Never fails; the `Result` matches the other backends' signature. Removing the global from each
  /// frame is best effort.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.exposed_fns.write().await.remove(name);
    let js = format!("() => {{ delete window['{name}'] }}");
    self.call_in_frames(&js, &json!([])).await;
    Ok(())
  }

  /// Call `function` in the document of every frame of this page; the
  /// page's own document alone if its frames cannot be listed.
  async fn call_in_frames(&self, function: &str, arguments: &serde_json::Value) {
    let contexts = match self.get_frame_tree().await {
      Ok(frames) => frames.into_iter().map(|frame| frame.frame_id).collect(),
      Err(_) => vec![self.context_id.to_string()],
    };
    for context in contexts {
      let _ = self
        .cmd(
          "script.callFunction",
          json!({
            "functionDeclaration": function,
            "arguments": arguments,
            "target": {"context": context},
            "awaitPromise": false,
            "resultOwnership": "none"
          }),
        )
        .await;
    }
  }
}
