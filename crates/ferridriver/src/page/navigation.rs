//! Page navigation (`goto`, `reload`, history, `content`, `set_content`).

use std::sync::Arc;

use crate::actions;
use crate::error::Result;
use crate::options::GotoOptions;

use super::Page;

impl Page {
  // ── Navigation ──────────────────────────────────────────────────────────

  /// Navigate to a URL with optional options (waitUntil, timeout).
  ///
  /// Returns the main-document `Response` when the backend can observe
  /// it, or `None` for same-document navigations (no new request was
  /// issued) / backends that genuinely cannot expose the main-document
  /// response (stock `WKWebView` has no public API for this). Mirrors
  /// Playwright's `Promise<Response | null>` contract on `page.goto`.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  #[track_caller]
  pub fn goto(
    self: &Arc<Self>,
    url: &str,
  ) -> crate::action::Action<'static, GotoOptions, Option<crate::network::Response>> {
    let page = Arc::clone(self);
    let url = url.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.goto_impl(&url, Some(opts)).await }))
  }

  /// Implementation of [`Self::goto`].
  #[tracing::instrument(skip(self, opts), fields(url))]
  pub(crate) async fn goto_impl(
    self: &Arc<Self>,
    url: &str,
    opts: Option<GotoOptions>,
  ) -> Result<Option<crate::network::Response>> {
    // Resolve against the context's `baseURL` option when set —
    // mirrors Playwright's `constructURLBasedOnBaseURL` applied in
    // `Page._goto` (`/tmp/playwright/packages/playwright-core/src/client/page.ts`).
    // Absolute URLs passthrough; relative paths resolve against baseURL.
    let resolved = self.resolve_with_base_url(url).await;
    let resolved = if resolved.starts_with("localhost") || resolved.starts_with("127.0.0.1") {
      format!("http://{resolved}")
    } else {
      resolved
    };
    let parsed_url = reqwest::Url::parse(&resolved)
      .map_err(|error| crate::error::FerriError::invalid_argument("url", error.to_string()))?;
    // A URL carrying embedded credentials (`https://user:pass@host`) populates
    // Chromium's context-scoped HTTP auth cache, which has no CDP clear API.
    // Taint the context so `contextReuse` disposes instead of recycling it.
    if (!parsed_url.username().is_empty() || parsed_url.password().is_some())
      && let Some(ctx) = self.context_ref.as_ref()
    {
      let state = ctx.state.read().await;
      state
        .http_authenticated
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(ctx.key.to_composite());
    }
    tracing::debug!(target: "ferridriver::action", action = "goto", url = %resolved, "page.goto");
    let trace_span = self.trace_span("goto", serde_json::json!({ "url": resolved }));
    let trace_span = crate::trace::open_action(self.snapshot_before(trace_span).await).await;
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    let referer = opts.as_ref().and_then(|o| o.referer.as_deref());
    let pre_nav = self.observed_lens();
    let result = crate::operation_budget::scoped(
      timeout,
      || format!("navigating to {resolved}"),
      self.inner.goto(&resolved, lifecycle, timeout, referer),
    )
    .await;
    if let Some(span) = trace_span {
      self.snapshot_after_and_finish(span, result.as_ref().err()).await;
    }
    self.slow_mo_pause("Page", "goto").await;
    // A goto that returned a document Response committed a NEW document
    // — deterministically advance the observed since-navigation window
    // past pre-nav entries even if the listener's `FrameNavigated` mark
    // was dropped by a lagged broadcast receiver. Same-document
    // navigations (fragment-only, `Response` = None) keep their window.
    if matches!(result, Ok(Some(_))) {
      self.raise_observed_nav_marks(pre_nav);
    }
    // BiDi subframe seeding: WebDriver BiDi's `browsingContext.contextCreated`
    // fires asynchronously for child iframes after `browsingContext.navigate`
    // completes, and the iframe's `name` attribute lives in the DOM —
    // not in the contextCreated payload. The listener-driven cache
    // therefore lags any synchronous `page.frame(name)` / `page.frames()`
    // call made right after `goto` returns. Mirror Playwright's
    // `bidiBrowser._onBrowsingContextCreated`
    // (`/tmp/playwright/packages/playwright-core/src/server/bidi/bidiBrowser.ts:146`)
    // by fetching the full subtree on the goto-return path and seeding
    // the wrapper's cache directly — `BidiPage::get_frame_tree` already
    // does the parallel `window.name` resolution for unnamed children,
    // so the wrapper sees a fully-populated cache.
    // Backends without per-frame attach/navigate events on the wire need an
    // explicit cache seed after navigation so synchronous `page.frame(...)`
    // / `page.frames()` calls see the iframes the test just navigated to.
    // - BiDi: `browsingContext.contextCreated` arrives async with empty
    //   `name`; the cache won't reflect the DOM until our `getTree` probe
    //   runs.
    // - WebKit (Playwright WebKit, cross-platform): no FrameAttached
    //   events at all — the cache is populated solely by `get_frame_tree`'s
    //   DOM probe, which must run after EVERY navigation to refresh the
    //   iframe set on a reused page.
    self.finish_navigation(result).await
  }

  async fn finish_navigation(
    &self,
    result: Result<Option<crate::network::Response>>,
  ) -> Result<Option<crate::network::Response>> {
    let response = result?;
    let needs_lightpanda_frame_sync = self.inner.is_lightpanda()
      && self.with_frame_cache(|c| {
        c.live_ids().any(|id| {
          c.record(&id)
            .is_some_and(|r| r.info.parent_frame_id.is_some() && r.info.name.is_empty())
        })
      });
    if matches!(
      self.inner.kind(),
      crate::backend::BackendKind::Bidi | crate::backend::BackendKind::WebKit | crate::backend::BackendKind::WebDriver
    ) || needs_lightpanda_frame_sync
    {
      // Single pass — extra sync rounds would push past the
      // `setTimeout(confirm, 80)` window dialog tests rely on between
      // goto-returning and the user subscribing to `waitForEvent`.
      // Stragglers get picked up via the live FrameAttached listener.
      self.sync_frames().await?;
    }
    Ok(response)
  }

  /// Resolve a user-supplied URL against the owning context's
  /// `baseURL` option. Returns the input unchanged when no context
  /// is attached, no `baseURL` is set, or the input is already
  /// absolute. See
  /// [`crate::options::construct_url_with_base`] for the resolution
  /// rules.
  pub(crate) async fn resolve_with_base_url(&self, url: &str) -> String {
    let Some(ctx) = self.context_ref.as_ref() else {
      return url.to_string();
    };
    let state = ctx.state.read().await;
    let Some(bag) = state.get_context_options(&ctx.key.to_composite()) else {
      return url.to_string();
    };
    crate::options::construct_url_with_base(bag.base_url.as_deref(), url)
  }

  /// Navigate back in history. Returns the main-document `Response` on
  /// the same basis as `goto` (or `None`).
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  #[track_caller]
  pub fn go_back(&self) -> crate::action::Action<'_, GotoOptions, Option<crate::network::Response>> {
    let page = self;
    crate::action::Action::new(move |opts| Box::pin(async move { page.go_back_impl(Some(opts)).await }))
  }

  /// Implementation of [`Self::go_back`].
  pub(crate) async fn go_back_impl(&self, opts: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    let trace_span = self.trace_span("goBack", serde_json::json!({}));
    let trace_span = crate::trace::open_action(self.snapshot_before(trace_span).await).await;
    let result = self
      .finish_navigation(
        crate::operation_budget::scoped(timeout, || "going back".into(), self.inner.go_back(lifecycle, timeout)).await,
      )
      .await;
    if let Some(span) = trace_span {
      self.snapshot_after_and_finish(span, result.as_ref().err()).await;
    }
    self.slow_mo_pause("Page", "goBack").await;
    result
  }

  /// Navigate forward in history. Returns the main-document `Response`
  /// on the same basis as `goto` (or `None`).
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or the wait condition times out.
  #[track_caller]
  pub fn go_forward(&self) -> crate::action::Action<'_, GotoOptions, Option<crate::network::Response>> {
    let page = self;
    crate::action::Action::new(move |opts| Box::pin(async move { page.go_forward_impl(Some(opts)).await }))
  }

  /// Implementation of [`Self::go_forward`].
  pub(crate) async fn go_forward_impl(&self, opts: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    let trace_span = self.trace_span("goForward", serde_json::json!({}));
    let trace_span = crate::trace::open_action(self.snapshot_before(trace_span).await).await;
    let result = self
      .finish_navigation(
        crate::operation_budget::scoped(
          timeout,
          || "going forward".into(),
          self.inner.go_forward(lifecycle, timeout),
        )
        .await,
      )
      .await;
    if let Some(span) = trace_span {
      self.snapshot_after_and_finish(span, result.as_ref().err()).await;
    }
    self.slow_mo_pause("Page", "goForward").await;
    result
  }

  /// Reload the current page. Returns the main-document `Response` on
  /// the same basis as `goto` (or `None`).
  ///
  /// # Errors
  ///
  /// Returns an error if the reload fails or the wait condition times out.
  #[track_caller]
  pub fn reload(&self) -> crate::action::Action<'_, GotoOptions, Option<crate::network::Response>> {
    let page = self;
    crate::action::Action::new(move |opts| Box::pin(async move { page.reload_impl(Some(opts)).await }))
  }

  /// Implementation of [`Self::reload`].
  pub(crate) async fn reload_impl(&self, opts: Option<GotoOptions>) -> Result<Option<crate::network::Response>> {
    let (lifecycle, timeout) = Self::resolve_nav_opts(opts.as_ref(), self.default_navigation_timeout());
    let trace_span = self.trace_span("reload", serde_json::json!({}));
    let trace_span = crate::trace::open_action(self.snapshot_before(trace_span).await).await;
    let pre_nav = self.observed_lens();
    let result =
      crate::operation_budget::scoped(timeout, || "reloading".into(), self.inner.reload(lifecycle, timeout)).await;
    let navigation_succeeded = result.is_ok();
    let result = self.finish_navigation(result).await;
    if let Some(span) = trace_span {
      self.snapshot_after_and_finish(span, result.as_ref().err()).await;
    }
    self.slow_mo_pause("Page", "reload").await;
    // A successful reload ALWAYS commits a new document — advance the
    // observed since-navigation window even when the listener's
    // `FrameNavigated` mark was dropped (lagged broadcast receiver).
    if navigation_succeeded {
      self.raise_observed_nav_marks(pre_nav);
    }
    result
  }

  /// Snapshot the observed console/error buffer lengths for the
  /// navigation-window raise on the API nav path.
  fn observed_lens(&self) -> (usize, usize) {
    match self.inner.observed().lock() {
      Ok(o) => o.lens(),
      Err(poisoned) => poisoned.into_inner().lens(),
    }
  }

  fn raise_observed_nav_marks(&self, pre_nav: (usize, usize)) {
    match self.inner.observed().lock() {
      Ok(mut o) => o.raise_nav_marks(pre_nav),
      Err(poisoned) => poisoned.into_inner().raise_nav_marks(pre_nav),
    }
  }

  /// Parse `GotoOptions` into backend `NavLifecycle` + timeout.
  fn resolve_nav_opts(opts: Option<&GotoOptions>, default_timeout: u64) -> (crate::backend::NavLifecycle, u64) {
    let wait_until = opts.and_then(|o| o.wait_until).unwrap_or_default();
    let timeout = opts.and_then(|o| o.timeout).unwrap_or(default_timeout);
    (
      crate::backend::NavLifecycle::parse_lifecycle(wait_until.as_str()),
      timeout,
    )
  }

  /// Get the current page URL — the main frame's URL.
  ///
  /// Playwright: [`page.url()`](https://playwright.dev/docs/api/class-page#page-url)
  /// is **synchronous** (`url(): string`). It reads the locally-tracked
  /// main-frame URL (kept current by navigation/lifecycle events), the
  /// same source [`crate::frame::Frame::url`] uses — no backend round-trip.
  #[must_use]
  pub fn url(&self) -> String {
    let url = self.with_frame_cache(|c| {
      c.record(&c.main_frame_id())
        .map(|r| r.info.url.clone())
        .unwrap_or_default()
    });
    // See [`Frame::url`]: no navigation has committed yet.
    if url.is_empty() {
      return String::from("about:blank");
    }
    url
  }

  /// Get the current page title.
  ///
  /// # Errors
  ///
  /// Returns an error if the title cannot be retrieved from the backend.
  pub async fn title(&self) -> Result<String> {
    self
      .traced("title", serde_json::json!({}), async {
        self.inner.title().await.map(std::option::Option::unwrap_or_default)
      })
      .await
  }

  // ── Content ─────────────────────────────────────────────────────────────

  /// Playwright: `page.content(options?: { includeShadow?: boolean }):
  /// Promise<string>`, the main frame's content.
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be retrieved.
  #[track_caller]
  pub fn content(self: &Arc<Self>) -> crate::action::Action<'_, crate::options::ContentOptions, String> {
    let page = self;
    crate::action::Action::new(move |opts: crate::options::ContentOptions| {
      Box::pin(async move {
        let params = serde_json::json!({ "includeShadow": opts.include_shadow });
        page
          .traced("content", params, page.main_frame().content_impl(opts))
          .await
      })
    })
  }

  /// Playwright: `page.setContent(html, options?: { timeout?,
  /// waitUntil? }): Promise<void>`. See [`crate::frame::Frame::set_content`],
  /// which this is for the main frame.
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be set or the document does
  /// not reach `waitUntil` in time.
  #[track_caller]
  pub fn set_content(
    self: &Arc<Self>,
    html: &str,
  ) -> crate::action::Action<'static, crate::options::SetContentOptions, ()> {
    let page = Arc::clone(self);
    let html = html.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        page
          .traced(
            "setContent",
            serde_json::json!({ "html": html }),
            page.main_frame().set_content_impl(&html, opts),
          )
          .await
      })
    })
  }

  /// Extract the page content as markdown.
  ///
  /// # Errors
  ///
  /// Returns an error if the markdown extraction fails.
  pub async fn markdown(&self) -> Result<String> {
    actions::extract_markdown(&self.inner).await
  }

  /// Get the text content of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn text_content(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, Option<String>> {
    self.main_frame().text_content(selector)
  }

  /// Get the inner text of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn inner_text(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, String> {
    self.main_frame().inner_text(selector)
  }

  /// Get the inner HTML of an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn inner_html(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, String> {
    self.main_frame().inner_html(selector)
  }

  /// Get an attribute value from an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn get_attribute(
    self: &Arc<Self>,
    selector: &str,
    name: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, Option<String>> {
    self.main_frame().get_attribute(selector, name)
  }

  /// Get the input value of a form element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn input_value(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, String> {
    self.main_frame().input_value(selector)
  }
}
