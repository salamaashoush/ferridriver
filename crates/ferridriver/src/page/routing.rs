//! Network/WebSocket routing, exposed functions, and script/style/init-script injection on [`Page`].

use std::sync::Arc;

use crate::error::Result;

use super::Page;

impl Page {
  // ── Network Interception ────────────────────────────────────────────────

  /// Intercept network requests matching a [`crate::url_matcher::UrlMatcher`].
  /// The handler receives a [`crate::route::Route`] and must call exactly one
  /// of `fulfill()`, `continue_route()`, or `abort()`.
  ///
  /// ```ignore
  /// use ferridriver::route::{Route, FulfillResponse};
  /// use ferridriver::url_matcher::UrlMatcher;
  /// use std::sync::Arc;
  ///
  /// // Mock an API endpoint
  /// page.route(UrlMatcher::glob("**/api/data")?, Arc::new(|route: Route| {
  ///     route.fulfill(FulfillResponse {
  ///         status: 200,
  ///         body: b"{\"mocked\": true}".to_vec(),
  ///         content_type: Some("application/json".into()),
  ///         ..Default::default()
  ///     });
  /// }), None).await?;
  ///
  /// // Block image loading
  /// page.route(UrlMatcher::glob("**/*.{png,jpg,gif}")?, Arc::new(|route: Route| {
  ///     route.abort(Some("blockedbyclient"));
  /// }), None).await?;
  /// ```
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` reverses
  /// the registration (equivalent to calling [`Page::unroute`] with the same
  /// matcher). Mirrors Playwright `page.route(...)` which returns a
  /// `DisposableStub` (`client/page.ts:535`).
  ///
  /// # Errors
  ///
  /// Returns an error if the route interception cannot be set up.
  pub async fn route(
    &self,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::route::RouteHandler,
    times: Option<u32>,
  ) -> Result<crate::disposable::Disposable> {
    self
      .inner
      .route(crate::route::RegisteredRoute::new(matcher.clone(), handler, times).with_client(self.http_client()))
      .await?;
    let inner = self.inner.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      inner.unroute(&matcher, crate::route::RouteScope::Page, None).await
    }))
  }

  /// Playwright: `page.routeFromHAR(har, options?)`. Replay recorded
  /// responses from a HAR file (plain `.har` or `.zip` archive) for
  /// matching requests, or with `update: true` record this page's
  /// traffic into the file instead (written when the owning context
  /// closes, like Playwright's `HarTracer` page-filtered recorder).
  ///
  /// # Errors
  ///
  /// Returns an error if the HAR file cannot be read/parsed, the route
  /// cannot be installed, or (`update: true`) the page is not bound to
  /// a context whose close can flush the recording.
  #[track_caller]
  pub fn route_from_har(
    &self,
    path: &std::path::Path,
  ) -> crate::action::Action<'_, crate::har::RouteFromHarOptions, ()> {
    let page = self;
    let path = path.to_path_buf();
    crate::action::Action::new(move |opts| Box::pin(async move { page.route_from_har_impl(&path, opts).await }))
  }

  /// Implementation of [`Self::route_from_har`].
  pub(crate) async fn route_from_har_impl(
    &self,
    path: &std::path::Path,
    options: crate::har::RouteFromHarOptions,
  ) -> Result<()> {
    if options.update {
      // Same registry + flush point as the context-scoped recorder
      // (context close), narrowed to this page's traffic via the
      // capture-time `page_guid` stamped on every logged request.
      let Some(ctx) = self.context() else {
        return Err(crate::error::FerriError::unsupported(
          "page.routeFromHAR({ update: true }) requires a page opened through a context",
        ));
      };
      return ctx
        .register_har_update_recorder(path, &options, Some(self.inner.page_guid()))
        .await;
    }
    let handler = crate::har::route_handler_from_file(path, options.not_found)?;
    let matcher = options.url.unwrap_or_else(crate::url_matcher::UrlMatcher::any);
    self
      .inner
      .route(crate::route::RegisteredRoute::new(matcher, handler, None))
      .await
  }

  /// Remove page-scoped routes registered for `matcher`. With a
  /// `handler_id` — [`crate::route::RegisteredRoute::handler_id`] of the
  /// registration to drop — only that one goes, which is what
  /// Playwright's `page.unroute(url, handler)` means; without one, every
  /// route sharing the matcher is removed.
  ///
  /// # Errors
  ///
  /// Returns an error if the route handlers cannot be removed.
  pub async fn unroute(&self, matcher: &crate::url_matcher::UrlMatcher, handler_id: Option<usize>) -> Result<()> {
    self
      .inner
      .unroute(matcher, crate::route::RouteScope::Page, handler_id)
      .await
  }

  /// Remove all route handlers registered via [`Page::route`].
  ///
  /// Mirrors Playwright's
  /// `page.unrouteAll({ behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// The `behavior` selects how to treat handlers still running when the
  /// call is made; ferridriver route handlers run synchronously inside the
  /// interception loop, so once the routes are cleared no detached handler
  /// task can still be in flight — every variant performs the same teardown
  /// (clear routes, disable interception).
  ///
  /// Context-scoped routes installed via `context.route` stay active,
  /// matching Playwright where `page.unrouteAll` only clears
  /// `page._routes`.
  ///
  /// # Errors
  ///
  /// Returns an error if the underlying interception teardown fails.
  pub async fn unroute_all(&self, behavior: Option<crate::options::UnrouteBehavior>) -> Result<()> {
    self
      .inner
      .unroute_all(behavior.unwrap_or_default(), Some(crate::route::RouteScope::Page))
      .await
  }

  /// Intercept WebSocket connections on this page that match `matcher`.
  /// Playwright: `page.routeWebSocket(url, handler)`. The handler is
  /// invoked with a live [`crate::web_socket_route::WebSocketRoute`] when
  /// a matching `new WebSocket(...)` is created in the page.
  ///
  /// # Errors
  ///
  /// Returns an error if installing the page mock / binding fails.
  pub async fn route_web_socket(
    self: &Arc<Self>,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::web_socket_route::WsHandler,
  ) -> Result<()> {
    self
      .route_web_socket_scoped(matcher, handler, crate::web_socket_route::WsRouteScope::Page)
      .await
  }

  /// `safeNonStallingEvaluateInAllFrames`: run `expression` in every
  /// frame's main world; a frame that cannot run it (it is navigating,
  /// or gone) is skipped.
  pub(crate) async fn evaluate_in_all_frames(&self, expression: &str) {
    let main = self.with_frame_cache(crate::frame_cache::FrameCache::main_frame_id);
    let ids: Vec<Arc<str>> = self.with_frame_cache(|c| c.live_ids().collect());
    let inner = &self.inner;
    let runs = ids.iter().map(|id| {
      let is_main = main == *id;
      async move {
        let _ = if is_main {
          inner.evaluate(expression).await
        } else {
          inner.evaluate_in_frame(expression, id).await
        };
      }
    });
    if ids.is_empty() {
      let _ = inner.evaluate(expression).await;
    } else {
      futures::future::join_all(runs).await;
    }
  }

  /// Install a WebSocket route at the given scope. `Page` scope routes
  /// are matched before `Context` scope routes at socket creation
  /// (Playwright's page-dispatcher-before-context-dispatcher order);
  /// the context layer uses `Context` scope both for its immediate
  /// fan-out and when re-applying registered routes to a fresh page.
  pub(crate) async fn route_web_socket_scoped(
    self: &Arc<Self>,
    matcher: crate::url_matcher::UrlMatcher,
    handler: crate::web_socket_route::WsHandler,
    scope: crate::web_socket_route::WsRouteScope,
  ) -> Result<()> {
    use crate::web_socket_route as wsr;
    let router = wsr::router_for_page(self.backend_page_id(), self.inner.clone());
    let first = router.add_route(matcher, handler, scope);
    if first {
      self
        .inner
        .expose_binding(wsr::WS_BINDING_NAME, wsr::binding_callback(router))
        .await?;
      let source = crate::options::evaluation_script(wsr::mock_init_script(), None)?;
      self.inner.add_init_script(&source).await?;
      // The documents already loaded get the mock too, as Playwright's
      // `WebSocketRouteDispatcher.install` does.
      self.evaluate_in_all_frames(&source).await;
    }
    Ok(())
  }

  // ── Exposed Functions ───────────────────────────────────────────────────

  /// Expose a Rust function to the page as `window.<name>(...)`.
  /// The page can call it as an async function and receive the return value.
  /// The exposed function persists across navigations.
  ///
  /// ```ignore
  /// use std::sync::Arc;
  ///
  /// page.expose_function("compute", Arc::new(|args| {
  ///     Box::pin(async move {
  ///         let x = args.first().and_then(|v| v.as_f64()).unwrap_or(0.0);
  ///         Ok(serde_json::json!(x * 2.0).into())
  ///     })
  /// })).await?;
  ///
  /// // In the page:
  /// // const result = await window.compute(21); // returns 42
  /// ```
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be exposed to the page.
  pub async fn expose_function(&self, name: &str, func: crate::events::ExposedFn) -> Result<()> {
    self.inner.expose_function(name, func).await
  }

  /// Expose a Rust binding to the page as `window.<name>(...)`. Like
  /// [`Self::expose_function`], but the callback additionally receives a
  /// [`crate::events::BindingSource`] identifying the calling context,
  /// page, and frame (Playwright: `page.exposeBinding(name, callback)`,
  /// `client/page.ts:371`). The page still calls it as an async function
  /// receiving the return value.
  ///
  /// # Errors
  ///
  /// Returns an error if the binding cannot be installed on the page.
  pub async fn expose_binding(&self, name: &str, binding: crate::events::ExposedBinding) -> Result<()> {
    self.inner.expose_binding(name, binding).await
  }

  /// Remove a previously exposed function.
  ///
  /// # Errors
  ///
  /// Returns an error if the function cannot be removed.
  pub async fn remove_exposed_function(&self, name: &str) -> Result<()> {
    self.inner.remove_exposed_function(name).await
  }

  // ── Script / Style injection ────────────────────────────────────────────

  /// Playwright: `page.addScriptTag(options?: { content?, path?, type?,
  /// url? }): Promise<ElementHandle>`. See
  /// [`crate::frame::Frame::add_script_tag`], which this is for the main
  /// frame.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the script fails to load.
  #[track_caller]
  pub fn add_script_tag(
    self: &Arc<Self>,
  ) -> crate::action::Action<'static, crate::options::AddScriptTagOptions, crate::element_handle::ElementHandle> {
    self.main_frame().add_script_tag()
  }

  /// Playwright: `page.addStyleTag(options?: { content?, path?, url? }):
  /// Promise<ElementHandle>`. See [`crate::frame::Frame::add_style_tag`],
  /// which this is for the main frame.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the stylesheet fails to load.
  #[track_caller]
  pub fn add_style_tag(
    self: &Arc<Self>,
  ) -> crate::action::Action<'static, crate::options::AddStyleTagOptions, crate::element_handle::ElementHandle> {
    self.main_frame().add_style_tag()
  }

  // ── Init Scripts ────────────────────────────────────────────────────────

  /// Register a script to run before any page JS on every navigation.
  /// Mirrors Playwright's `page.addInitScript(script, arg)` from
  /// `/tmp/playwright/packages/playwright-core/src/client/page.ts:520`.
  ///
  /// Accepts the full Playwright argument shape: a JS function body
  /// (pre-serialised via `fn.toString()` at the binding layer), a verbatim
  /// source string, a `{ path }`, or a `{ content }`. The optional `arg`
  /// is JSON-stringified and composed into a `(body)(arg)` wrapper for
  /// the `Function` variant; passing `arg` alongside any non-function
  /// variant is a Playwright-parity error (see [`crate::options::evaluation_script`]).
  ///
  /// Returns a [`crate::disposable::Disposable`] whose `dispose()` removes the
  /// injected script (equivalent to [`Page::remove_init_script`] with the
  /// generated identifier). Mirrors Playwright `page.addInitScript(...)` which
  /// returns a `Disposable` (`client/page.ts:532`).
  ///
  /// # Errors
  ///
  /// Returns an error if `evaluation_script` lowering fails (bad path, bad
  /// arg combination, JSON serialisation) or the backend injection fails.
  pub async fn add_init_script(
    &self,
    script: crate::options::InitScriptSource,
    arg: Option<serde_json::Value>,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script(script, arg.as_ref())?;
    let identifier = self.inner.add_init_script(&source).await?;
    let inner = self.inner.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      inner.remove_init_script(&identifier).await
    }))
  }

  /// Playwright: `page.addInitScript(script, arg, { exposeFunctions: true })`.
  /// Each function in `arg` stays callable from every document the
  /// script runs in, until the returned disposable removes the script.
  ///
  /// # Errors
  ///
  /// Returns an error unless `script` is a function, if `arg` holds a
  /// handle, or if the backend injection fails.
  pub async fn add_init_script_exposing_functions(
    &self,
    script: crate::options::InitScriptSource,
    arg: crate::protocol::SerializedArgument,
  ) -> Result<crate::disposable::Disposable> {
    let source = crate::options::evaluation_script_exposing(script, &arg.value)?;
    for function in &arg.functions {
      self
        .inner
        .expose_callback(&function.name, Arc::clone(&function.binding))
        .await?;
    }
    let identifier = self.inner.add_init_script(&source).await?;
    let inner = self.inner.clone();
    let names: Vec<String> = arg.functions.into_iter().map(|f| f.name).collect();
    Ok(crate::disposable::Disposable::new(move || async move {
      for name in &names {
        inner.remove_callback(name).await;
      }
      inner.remove_init_script(&identifier).await
    }))
  }

  /// Remove a previously injected init script by identifier.
  ///
  /// # Errors
  ///
  /// Returns an error if the init script cannot be removed.
  pub async fn remove_init_script(&self, identifier: &str) -> Result<()> {
    self.inner.remove_init_script(identifier).await
  }
}
