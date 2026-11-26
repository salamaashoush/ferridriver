//! Context-level events, cookies, routes, emulation, and storage APIs.

use std::sync::Arc;

use crate::error::Result;
use crate::page::Page;

use super::{ContextRef, bind_source};

impl ContextRef {
  // ── Context-level events ────────────────────────────────────────────────

  /// Register a context-level event listener. Supported events:
  /// `'weberror'` (unhandled errors / rejections on any page in this
  /// context — mirrors Playwright's
  /// `browserContext.on('weberror', (webError: WebError) => ...)`).
  pub fn on(&self, event_name: &str, callback: crate::events::ContextEventCallback) -> crate::events::ListenerId {
    self.events.on(event_name, callback)
  }

  /// One-shot context-level listener — see [`Self::on`].
  pub fn once(&self, event_name: &str, callback: crate::events::ContextEventCallback) -> crate::events::ListenerId {
    self.events.once(event_name, callback)
  }

  /// Remove a previously registered context-level listener.
  pub fn off(&self, id: crate::events::ListenerId) {
    self.events.off(id);
  }

  /// Node's `prependListener`: run before every listener already
  /// attached to `event_name`.
  pub fn prepend_listener(
    &self,
    event_name: &str,
    callback: crate::events::ContextEventCallback,
  ) -> crate::events::ListenerId {
    self.events.prepend(event_name, callback)
  }

  /// Node's `prependOnceListener`.
  pub fn prepend_once_listener(
    &self,
    event_name: &str,
    callback: crate::events::ContextEventCallback,
  ) -> crate::events::ListenerId {
    self.events.prepend_once(event_name, callback)
  }

  /// Remove every context-level listener.
  pub fn remove_all_listeners(&self) {
    self.events.remove_all_listeners();
  }

  /// Remove every context-level listener for `event_name`.
  pub fn remove_listeners_named(&self, event_name: &str) {
    self.events.remove_listeners_named(event_name);
  }

  /// Node's `listenerCount(type)`.
  #[must_use]
  pub fn listener_count(&self, event_name: &str) -> usize {
    self.events.listener_count(event_name)
  }

  /// Node's `eventNames()`.
  #[must_use]
  pub fn event_names(&self) -> Vec<String> {
    self.events.event_names()
  }

  /// The listener ids registered for `event_name`, in dispatch order.
  #[must_use]
  pub fn listener_ids_named(&self, event_name: &str) -> Vec<crate::events::ListenerId> {
    self.events.listener_ids_named(event_name)
  }

  /// Node's `setMaxListeners(n)`; `0` disables the leak warning.
  pub fn set_max_listeners(&self, max: usize) {
    self.events.set_max_listeners(max);
  }

  /// Node's `getMaxListeners()`.
  #[must_use]
  pub fn max_listeners(&self) -> usize {
    self.events.max_listeners()
  }

  /// Wait for the next context-level event matching `event_name`, with
  /// `timeout_ms`. Mirrors Playwright's
  /// `browserContext.waitForEvent(event, options?)`.
  /// Listens from the call, so an event the caller triggers before
  /// awaiting is not missed.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the event channel is closed.
  pub fn wait_for_event(
    &self,
    event_name: &str,
    timeout_ms: u64,
  ) -> impl std::future::Future<Output = Result<crate::events::ContextEvent>> + Send + use<> {
    self.events.wait_for_event(event_name, timeout_ms)
  }

  // ── Context-level APIs (apply to all pages) ────────────────────────────

  /// Add an init script to all pages in this context (current + future).
  /// Mirrors Playwright's `browserContext.addInitScript(script, arg)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:356`.
  /// See [`crate::page::Page::add_init_script`] for argument semantics.
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the
  /// injected script from every page it was added to. Mirrors Playwright
  /// `browserContext.addInitScript(...)` which returns a `DisposableObject`
  /// (`client/browserContext.ts:361`).
  ///
  /// # Errors
  ///
  /// Returns an error if `evaluation_script` lowering fails, the context
  /// does not exist, or script injection fails on any page.
  pub async fn add_init_script(
    &self,
    script: crate::options::InitScriptSource,
    arg: Option<serde_json::Value>,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script(script, arg.as_ref())?;
    self
      .traced(
        "addInitScript",
        serde_json::json!({}),
        self.add_init_script_source(source),
      )
      .await
  }

  /// Playwright: `browserContext.addInitScript(script, arg, {
  /// exposeFunctions: true })`. Each function in `arg` stays callable
  /// from every page of the context, popups included, until the returned
  /// disposable removes the script.
  ///
  /// # Errors
  ///
  /// Returns an error unless `script` is a function, if `arg` holds a
  /// handle, or if injection fails on any page.
  pub async fn add_init_script_exposing_functions(
    &self,
    script: crate::options::InitScriptSource,
    arg: crate::protocol::SerializedArgument,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script_exposing(script, &arg.value)?;
    let composite = self.key.to_composite();
    {
      let callbacks = Arc::clone(&self.state.read().await.context_callbacks);
      let mut guard = callbacks.write().await;
      let entry = guard.entry(composite.clone()).or_default();
      for function in &arg.functions {
        entry.insert(function.name.clone(), Arc::clone(&function.binding));
      }
    }
    let pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in &pages {
      for function in &arg.functions {
        page
          .expose_context_callback(
            &function.name,
            bind_source(Arc::clone(&function.binding), composite.clone()),
          )
          .await?;
      }
    }
    let script = self
      .traced(
        "addInitScript",
        serde_json::json!({}),
        self.add_init_script_source(source),
      )
      .await?;
    let this = self.clone();
    let names: Vec<String> = arg.functions.into_iter().map(|f| f.name).collect();
    Ok(crate::disposable::Disposable::new(move || async move {
      {
        let callbacks = Arc::clone(&this.state.read().await.context_callbacks);
        let mut guard = callbacks.write().await;
        if let Some(map) = guard.get_mut(&this.key.to_composite()) {
          for name in &names {
            map.remove(name);
          }
        }
      }
      let pages = {
        let state = this.state.read().await;
        state.context(&this.name).map(|c| c.pages.clone()).unwrap_or_default()
      };
      for page in &pages {
        for name in &names {
          page.remove_callback(name).await;
        }
      }
      script.dispose().await
    }))
  }

  /// Register a lowered init-script source on this context: applied to
  /// every open page now, recorded in the per-context registry so
  /// [`Self::new_page`] applies it to future pages too (Playwright
  /// context init scripts are current + future).
  pub(crate) async fn add_init_script_source(&self, source: String) -> Result<crate::disposable::Disposable> {
    let composite = self.key.to_composite();
    let (registry, registry_id) = {
      let state = self.state.read().await;
      let id = state
        .context_init_script_counter
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
      (Arc::clone(&state.context_init_scripts), id)
    };
    let installed = Arc::new(std::sync::Mutex::new(Vec::new()));
    registry
      .write()
      .await
      .entry(composite.clone())
      .or_default()
      .push(crate::state::ContextInitScript {
        id: registry_id,
        source: source.clone(),
        installed: Arc::clone(&installed),
      });

    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in inner_pages {
      let id = page.add_context_init_script(registry_id, &source).await?;
      installed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((page, id));
    }
    Ok(crate::disposable::Disposable::new(move || async move {
      {
        let mut guard = registry.write().await;
        if let Some(entries) = guard.get_mut(&composite) {
          entries.retain(|entry| entry.id != registry_id);
        }
      }
      let undo = std::mem::take(&mut *installed.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
      for (page, id) in undo {
        if !page.is_closed() {
          page.remove_init_script(&id).await?;
        }
      }
      Ok(())
    }))
  }

  /// Undo what callers registered on the context through each
  /// registration's own removal path, while its pages are still open to
  /// be told. Dropping the registries and closing the pages is not
  /// enough where the browser holds a registration for the context
  /// rather than the page: a Firefox preload script outlives every page
  /// it ran in.
  ///
  /// Options a context can only be created with (locale, user agent and
  /// the like) have no removal path and are not undone here; a backend
  /// that scopes one to its context keeps it until the next options set
  /// that field.
  pub(crate) async fn release_registrations(&self) -> Result<()> {
    let composite = self.key.to_composite();
    // Only what was registered is undone: a backend that cannot route at
    // all refuses to unroute too, and has no route to remove.
    let routed = {
      let registry = self.state.read().await.context_routes_handle();
      let guard = registry.read().await;
      guard.contains_key(&composite)
    };
    if routed {
      self.unroute_all(None).await?;
    }

    let bindings: Vec<String> = {
      let registry = self.state.read().await.context_bindings_handle();
      let guard = registry.read().await;
      guard
        .get(&composite)
        .map(|names| names.keys().cloned().collect())
        .unwrap_or_default()
    };
    for name in bindings {
      self.remove_exposed_binding(&name).await?;
    }

    let scripts = {
      let registry = Arc::clone(&self.state.read().await.context_init_scripts);
      registry.write().await.remove(&composite).unwrap_or_default()
    };
    for script in scripts {
      let installed = std::mem::take(
        &mut *script
          .installed
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner),
      );
      for (page, id) in installed {
        if !page.is_closed() {
          page.remove_init_script(&id).await?;
        }
      }
    }

    let options = self.state.read().await.get_context_options(&composite);
    if let Some(options) = options {
      if options.geolocation.is_some() {
        self.set_geolocation(None).await?;
      }
      if options.offline == Some(true) {
        self.set_offline(false).await?;
      }
      if options.network_conditions.is_some_and(|c| c.is_throttled()) {
        self.set_network_conditions(None).await?;
      }
      if options.cpu_throttling_rate.is_some_and(|r| r > 1.0) {
        self.set_cpu_throttling(None).await?;
      }
      if options.extra_http_headers.is_some_and(|headers| !headers.is_empty()) {
        self.set_extra_http_headers(&rustc_hash::FxHashMap::default()).await?;
      }
      if options.http_credentials.is_some() {
        self.set_http_credentials(None).await?;
      }
    }
    Ok(())
  }

  /// Install every registered context-level init script onto a fresh
  /// page.
  pub(super) async fn apply_context_init_scripts(&self, page: &Arc<Page>) -> Result<()> {
    let scripts: Vec<_> = {
      let registry = Arc::clone(&self.state.read().await.context_init_scripts);
      let guard = registry.read().await;
      guard
        .get(&self.key.to_composite())
        .map(|entries| {
          entries
            .iter()
            .map(|e| (e.id, e.source.clone(), Arc::clone(&e.installed)))
            .collect()
        })
        .unwrap_or_default()
    };
    for (key, source, installed) in scripts {
      let id = page.inner().add_context_init_script(key, &source).await?;
      installed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((page.inner().clone(), id));
    }
    Ok(())
  }

  /// Playwright: `browserContext.setGeolocation(geolocation | null)` —
  /// mutates the options bag and re-applies to every open page; `None`
  /// clears the override.
  ///
  /// # Errors
  ///
  /// Returns an error if re-application fails on any page.
  pub async fn set_geolocation(&self, geolocation: Option<crate::options::Geolocation>) -> Result<()> {
    self
      .traced(
        "setGeolocation",
        serde_json::json!({ "geolocation": geolocation.is_some() }),
        self.mutate_options(|o| o.geolocation = geolocation),
      )
      .await
  }

  /// Playwright: `browserContext.setExtraHTTPHeaders(headers)`.
  ///
  /// # Errors
  ///
  /// Returns an error if re-application fails on any page.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    let names: Vec<&String> = headers.keys().collect();
    let params = serde_json::json!({ "headers": names });
    let headers = headers.clone();
    self
      .traced(
        "setExtraHTTPHeaders",
        params,
        self.mutate_options(|o| o.extra_http_headers = Some(headers)),
      )
      .await
  }

  /// Playwright: `browserContext.setOffline(offline)`.
  ///
  /// # Errors
  ///
  /// Returns an error if re-application fails on any page.
  pub async fn set_offline(&self, offline: bool) -> Result<()> {
    self
      .traced(
        "setOffline",
        serde_json::json!({ "offline": offline }),
        self.mutate_options(|o| o.offline = Some(offline)),
      )
      .await
  }

  /// Throttles the network of every page in the context, open and
  /// future; `None` lifts it. Not in Playwright. Chromium only.
  ///
  /// # Errors
  ///
  /// Invalid numbers, a backend that cannot throttle, or a failure
  /// applying the conditions to an open page.
  pub async fn set_network_conditions(&self, conditions: Option<crate::options::NetworkConditions>) -> Result<()> {
    let conditions = conditions.unwrap_or(crate::options::NetworkConditions::NONE);
    conditions.validate()?;
    let params = serde_json::json!({
      "latency": conditions.latency,
      "downloadThroughput": conditions.download_throughput,
      "uploadThroughput": conditions.upload_throughput,
    });
    self
      .traced(
        "setNetworkConditions",
        params,
        self.mutate_options(|o| o.network_conditions = Some(conditions)),
      )
      .await
  }

  /// Slows every page's CPU by `rate` (`4.0` is four times slower);
  /// `None` lifts it. Not in Playwright. Chromium only.
  ///
  /// # Errors
  ///
  /// A rate below 1, a backend that cannot throttle, or a failure
  /// applying it to an open page.
  pub async fn set_cpu_throttling(&self, rate: Option<f64>) -> Result<()> {
    let rate = rate.unwrap_or(1.0);
    if !(rate.is_finite() && rate >= 1.0) {
      return Err(crate::error::FerriError::invalid_argument(
        "rate",
        format!("must be a number of at least 1 (1 is no slowdown), got {rate}"),
      ));
    }
    self
      .traced(
        "setCPUThrottling",
        serde_json::json!({ "rate": rate }),
        self.mutate_options(|o| o.cpu_throttling_rate = Some(rate)),
      )
      .await
  }

  /// Playwright: `browserContext.setHTTPCredentials(httpCredentials:
  /// HttpCredentials | HttpCredentials[] | null)`
  /// (`packages/playwright-core/src/client/browserContext.ts`). A
  /// challenge is answered with the first entry bound to no origin or to
  /// the request's.
  /// Stores the credentials on the context options bag so pages opened
  /// later inherit them, and applies them to every already-open page.
  /// Passing `None` clears stored credentials — future 401 challenges
  /// then surface as the browser's native auth dialog rather than being
  /// answered automatically.
  ///
  /// The bag mutation alone cannot express "clear" (the per-field
  /// `apply_context_options` future is keyed on `Some`), so this drives
  /// the dedicated [`crate::Page::set_http_credentials`] backend path on
  /// each open page directly.
  ///
  /// # Errors
  ///
  /// Returns an error if any open page's backend rejects the change
  /// (e.g. a backend that does not support auth-challenge interception).
  pub async fn set_http_credentials(&self, credentials: Option<Vec<crate::options::HttpCredentials>>) -> Result<()> {
    self
      .traced(
        "setHTTPCredentials",
        serde_json::json!({ "httpCredentials": credentials.is_some() }),
        self.set_http_credentials_untraced(credentials),
      )
      .await
  }

  async fn set_http_credentials_untraced(
    &self,
    credentials: Option<Vec<crate::options::HttpCredentials>>,
  ) -> Result<()> {
    // An empty list is no credentials, as in Playwright's
    // `toHttpCredentialsProtocol`.
    let credentials = credentials.filter(|list| !list.is_empty());
    let composite = self.key.to_composite();
    {
      let state = self.state.read().await;
      if credentials.is_some() {
        state
          .http_authenticated
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .insert(composite.clone());
      }
      let mut opts = state.get_context_options(&composite).unwrap_or_default();
      opts.http_credentials.clone_from(&credentials);
      state.set_context_options(&composite, opts);
    }
    let pages = self.discover_pages().await?;
    for page in pages {
      page.set_http_credentials(credentials.clone()).await?;
    }
    Ok(())
  }

  /// Register a route handler for all pages in this context — current
  /// AND future (the registration lives in the per-context registry and
  /// is re-applied by [`Self::new_page`], matching Playwright's
  /// context-scoped `_routes` list). The `times` budget is shared
  /// context-wide: requests from any page of the context consume the
  /// same counter.
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the
  /// handler from the registry and every page (equivalent to
  /// [`Self::unroute`]). Mirrors Playwright `browserContext.route(...)`
  /// which returns a `DisposableStub` (`client/browserContext.ts:377`).
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route registration fails.
  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
    times: Option<u32>,
  ) -> Result<crate::disposable::Disposable> {
    let params = serde_json::json!({ "url": matcher.describe() });
    self
      .traced("route", params, async {
        let registration = crate::route::RegisteredRoute::context_scoped(matcher.clone(), handler, times)
          .with_client(Some(self.http_client()));
        Box::pin(self.install_context_route(registration)).await?;
        let ctx = self.clone();
        Ok(crate::disposable::Disposable::new(move || async move {
          ctx.unroute(&matcher, None).await
        }))
      })
      .await
  }

  /// Register a context-scoped route: push into the per-context registry
  /// and fan a clone (sharing the `times` budget) onto every open page.
  async fn install_context_route(&self, registration: crate::route::RegisteredRoute) -> Result<()> {
    {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      guard
        .entry(self.key.to_composite())
        .or_default()
        .push(registration.clone());
    }
    // A context with no pages yet isn't registered in state (it
    // materialises on first `new_page`) — the registry entry above is
    // all that's needed; `new_page` re-applies it. Same tolerance as
    // `expose_binding` / `route_web_socket`.
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in inner_pages {
      page.route(registration.clone()).await?;
    }
    Ok(())
  }

  /// Install every registered context-level route onto a fresh page.
  /// Clones share the registry entry's `times` budget; entries whose
  /// budget is already exhausted are pruned instead of copied.
  pub(super) async fn apply_context_routes(&self, page: &Arc<Page>) -> Result<()> {
    let routes = {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      match guard.get_mut(&self.key.to_composite()) {
        Some(entries) => {
          entries.retain(crate::route::RegisteredRoute::live);
          entries.clone()
        },
        None => return Ok(()),
      }
    };
    for registration in routes {
      page.inner().route(registration).await?;
    }
    Ok(())
  }

  /// Playwright: `browserContext.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `matcher` on every page in this
  /// context — current AND future (the route is registered in the
  /// per-context registry and re-applied by [`Self::new_page`], matching
  /// Playwright's context-scoped interception patterns). Page-level
  /// routes take precedence over context-level routes at socket
  /// creation.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or installing the page
  /// mock / binding fails on any page.
  pub async fn route_web_socket(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::web_socket_route::WsHandler,
  ) -> Result<()> {
    {
      let registry = self.state.read().await.context_ws_routes_handle();
      let mut guard = registry.write().await;
      guard
        .entry(self.key.to_composite())
        .or_default()
        .push((matcher.clone(), Arc::clone(&handler)));
    }
    // A context with no pages yet isn't registered in state (it
    // materialises on first `new_page`) — the registry entry above is
    // all that's needed; `new_page` re-applies it. Same tolerance as
    // `expose_binding`.
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for inner in inner_pages {
      let page = Page::with_context(inner, self.clone());
      page
        .route_web_socket_scoped(
          matcher.clone(),
          Arc::clone(&handler),
          crate::web_socket_route::WsRouteScope::Context,
        )
        .await?;
    }
    Ok(())
  }

  /// Playwright: `browserContext.routeFromHAR(har, options?)`. Replays a HAR
  /// file across every page in this context — current and future.
  /// Replay-only; recording (`update: true`) is unsupported.
  ///
  /// # Errors
  ///
  /// Returns an error if the HAR file cannot be read/parsed or routes fail
  /// to install.
  #[track_caller]
  pub fn route_from_har(
    &self,
    path: &std::path::Path,
  ) -> crate::action::Action<'static, crate::har::RouteFromHarOptions, ()> {
    let ctx = self.clone();
    let path = path.to_path_buf();
    crate::action::Action::new(move |opts| Box::pin(async move { ctx.route_from_har_impl(&path, opts).await }))
  }

  pub(crate) async fn route_from_har_impl(
    &self,
    path: &std::path::Path,
    options: crate::har::RouteFromHarOptions,
  ) -> Result<()> {
    if options.update {
      return self.register_har_update_recorder(path, &options, None).await;
    }
    let handler = crate::har::route_handler_from_file(path, options.not_found)?;
    let matcher = options.url.unwrap_or_else(crate::url_matcher::UrlMatcher::any);
    Box::pin(self.install_context_route(crate::route::RegisteredRoute::context_scoped(matcher, handler, None))).await
  }

  /// Register a `routeFromHAR({ update: true })` recorder, flushed when
  /// this context closes. Playwright defaults
  /// (`client/tracing.ts:131-135`): content `attach`, mode `minimal`.
  /// `page_filter` narrows the recording to one page's traffic (the
  /// page-scoped `page.routeFromHAR` form).
  pub(crate) async fn register_har_update_recorder(
    &self,
    path: &std::path::Path,
    options: &crate::har::RouteFromHarOptions,
    page_filter: Option<String>,
  ) -> Result<()> {
    let start_len = match self.network_log_handle().await {
      Some(log) => log.read().await.len(),
      None => 0,
    };
    let recorder = crate::tracing::HarRecorder {
      path: path.to_path_buf(),
      content: options
        .update_content
        .unwrap_or(crate::tracing::HarContentPolicy::Attach),
      mode: options.update_mode.unwrap_or(crate::tracing::HarMode::Minimal),
      url_filter: options.url.clone().unwrap_or_else(crate::url_matcher::UrlMatcher::any),
      resources_dir: None,
      start_len,
      page_filter,
    };
    let registry = Arc::clone(&self.state.read().await.context_har_updates);
    registry
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .entry(self.key.to_composite())
      .or_default()
      .push(recorder);
    Ok(())
  }

  /// Remove context-scoped route handlers matching the given matcher
  /// from the registry and from all pages. Page-scoped routes with the
  /// same matcher stay active, matching Playwright where
  /// `context.unroute` only touches `context._routes`.
  ///
  /// # Errors
  ///
  /// Returns an error if the context does not exist or route removal fails.
  /// With a `handler_id` — [`crate::route::RegisteredRoute::handler_id`]
  /// of the registration to drop — only that one goes, which is what
  /// Playwright's `browserContext.unroute(url, handler)` means.
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher, handler_id: Option<usize>) -> Result<()> {
    self
      .traced(
        "unroute",
        serde_json::json!({ "url": matcher.describe() }),
        self.unroute_untraced(matcher, handler_id),
      )
      .await
  }

  async fn unroute_untraced(&self, matcher: &crate::url_matcher::UrlMatcher, handler_id: Option<usize>) -> Result<()> {
    {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      if let Some(entries) = guard.get_mut(&self.key.to_composite()) {
        entries.retain(|r| !r.matcher.equivalent(matcher) || handler_id.is_some_and(|id| r.handler_id() != id));
      }
    }
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in inner_pages {
      page
        .unroute(matcher, crate::route::RouteScope::Context, handler_id)
        .await?;
    }
    Ok(())
  }

  /// Remove all context-scoped route handlers from the registry and
  /// from every page. Page-scoped routes stay active. Mirrors
  /// Playwright's
  /// `browserContext.unrouteAll({ behavior?: 'wait' | 'ignoreErrors' | 'default' })`;
  /// ferridriver route handlers resolve synchronously inside the
  /// interception chain, so every `behavior` variant performs the same
  /// teardown.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying interception teardown fails.
  pub async fn unroute_all(&self, behavior: Option<crate::options::UnrouteBehavior>) -> Result<()> {
    {
      let registry = self.state.read().await.context_routes_handle();
      let mut guard = registry.write().await;
      guard.remove(&self.key.to_composite());
    }
    let inner_pages = {
      let state = self.state.read().await;
      state.context(&self.name).map(|c| c.pages.clone()).unwrap_or_default()
    };
    for page in inner_pages {
      page
        .unroute_all(behavior.unwrap_or_default(), Some(crate::route::RouteScope::Context))
        .await?;
    }
    Ok(())
  }
}
