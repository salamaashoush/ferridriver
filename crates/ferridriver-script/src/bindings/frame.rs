//! `FrameJs`: JS wrapper around `ferridriver::Frame`.
//!
//! Mirrors Playwright's
//! [Frame](https://playwright.dev/docs/api/class-frame) sync navigation
//! tree API — `name()`, `url()`, `isMainFrame()`, `parentFrame()`,
//! `childFrames()`, `isDetached()` — plus the small set of async
//! accessors needed for writing scripts that deal with iframes
//! (evaluate / title / content, locator).
//!
//! Action methods (`click`, `fill`, `hover`, etc.) and the full
//! getBy* option surface ship in **task 3.9** (Frame action methods).
//!
//! The underlying `ferridriver::Frame` is a cheap `(Arc<Page>, Arc<str>)`
//! handle — cloning it is free. All name/url/parent/children/detached
//! state is read live from the page-owned frame cache (see
//! `crate::frame_cache::FrameCache`) seeded at `Page::init_frame_cache`.

use ferridriver::Frame;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;
use rquickjs::function::Opt;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::{extract_page_function, quickjs_arg_to_serialized, serialized_value_to_quickjs};
use crate::bindings::locator::LocatorJs;

/// JS-visible wrapper around [`ferridriver::Frame`]. Constructed only by
/// `PageJs` / other `FrameJs` instances (`mainFrame`, `frames`, `frame`,
/// `parentFrame`, `childFrames`).
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Frame")]
pub struct FrameJs {
  #[qjs(skip_trace)]
  inner: Frame,
}

impl FrameJs {
  #[must_use]
  pub(crate) fn new(inner: Frame) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl FrameJs {
  #[qjs(get)]
  #[must_use]
  pub fn webmcp(&self) -> crate::bindings::web_mcp::WebMcpJs {
    crate::bindings::web_mcp::WebMcpJs::new(self.inner.webmcp())
  }

  // ── Sync frame-tree accessors (Playwright parity, task 3.8) ────────

  /// Frame name (from the `<iframe name=...>` attribute). Sync.
  #[qjs(rename = "name")]
  #[must_use]
  pub fn name(&self) -> String {
    self.inner.name()
  }

  /// Frame URL. Sync.
  #[qjs(rename = "url")]
  #[must_use]
  pub fn url(&self) -> String {
    self.inner.url()
  }

  /// True when this is the top-level page frame. Sync.
  #[qjs(rename = "isMainFrame")]
  #[must_use]
  pub fn is_main_frame(&self) -> bool {
    self.inner.is_main_frame()
  }

  /// Parent frame (null for the main frame). Sync.
  ///
  /// # Errors
  ///
  /// Throws when the engine cannot create or restore the `Frame` object.
  #[qjs(rename = "parentFrame")]
  pub fn parent_frame<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<rquickjs::Class<'js, FrameJs>>> {
    let parent = self.inner.parent_frame();
    Ok(crate::bindings::convert::Null(
      parent
        .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
        .transpose()?,
    ))
  }

  /// Child frames of this frame. Sync.
  ///
  /// # Errors
  ///
  /// Throws when the engine cannot create or restore a `Frame` object.
  #[qjs(rename = "childFrames")]
  pub fn child_frames<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<Vec<rquickjs::Class<'js, FrameJs>>> {
    self
      .inner
      .child_frames()
      .into_iter()
      .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
      .collect()
  }

  /// Whether this frame has been detached from the page. Sync.
  #[qjs(rename = "isDetached")]
  #[must_use]
  pub fn is_detached(&self) -> bool {
    self.inner.is_detached()
  }

  // ── Evaluation (frame-scoped) ──────────────────────────────────────

  /// Playwright: `frame.evaluate(pageFunction, arg?): Promise<R>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `arg` is cyclic, nested too deeply or out of range, an `Error`
  /// when `arg` holds a function and `options.exposeFunctions` is unset, and with the core error
  /// when the page function throws or the page is gone.
  #[qjs(rename = "evaluate")]
  pub async fn evaluate<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let (source, is_fn) = extract_page_function(&ctx, page_function)?;
        let serialized = crate::bindings::convert::quickjs_arg_with_functions(
          &ctx,
          arg.0,
          crate::bindings::convert::ArgFunctions::from_options(
            options.0.as_ref(),
            Some(self.inner.page_arc().backend_page_id()),
          )?,
        )?;
        let result = self
          .inner
          .evaluate(&source, serialized, is_fn)
          .await
          .into_js_with(&ctx)?;
        serialized_value_to_quickjs(&ctx, &result)
      })
      .await
  }

  /// Playwright: `frame.$eval(selector, pageFunction, arg?): Promise<R>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `arg` is cyclic, nested too deeply or out of range, an `Error`
  /// when `arg` holds a function, and with the core error when no element matches or the page
  /// function throws.
  #[qjs(rename = "$eval")]
  pub async fn eval_on_selector<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let (source, is_fn) = extract_page_function(&ctx, page_function)?;
        let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
        let result = self
          .inner
          .eval_on_selector(&selector, &source, serialized, is_fn)
          .await
          .into_js_with(&ctx)?;
        serialized_value_to_quickjs(&ctx, &result)
      })
      .await
  }

  /// Playwright: `frame.$$eval(selector, pageFunction, arg?): Promise<R>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `arg` is cyclic, nested too deeply or out of range, an `Error`
  /// when `arg` holds a function, and with the core error when the page function throws or the page
  /// is gone.
  #[qjs(rename = "$$eval")]
  pub async fn eval_on_selector_all<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<rquickjs::Value<'js>> {
    call_site
      .scope(async move {
        let (source, is_fn) = extract_page_function(&ctx, page_function)?;
        let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
        let result = self
          .inner
          .eval_on_selector_all(&selector, &source, serialized, is_fn)
          .await
          .into_js_with(&ctx)?;
        serialized_value_to_quickjs(&ctx, &result)
      })
      .await
  }

  /// Playwright: `frame.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `arg` is cyclic, nested too deeply or out of range, an `Error`
  /// when `arg` holds a function and `options.exposeFunctions` is unset, and with the core error
  /// when the page function throws or the page is gone.
  #[qjs(rename = "evaluateHandle")]
  pub async fn evaluate_handle<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::js_handle::JSHandleJs> {
    call_site
      .scope(async move {
        let (source, is_fn) = extract_page_function(&ctx, page_function)?;
        let serialized = crate::bindings::convert::quickjs_arg_with_functions(
          &ctx,
          arg.0,
          crate::bindings::convert::ArgFunctions::from_options(
            options.0.as_ref(),
            Some(self.inner.page_arc().backend_page_id()),
          )?,
        )?;
        let handle = self
          .inner
          .evaluate_handle(&source, serialized, is_fn)
          .await
          .into_js_with(&ctx)?;
        Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
      })
      .await
  }

  /// Playwright's `frame.title()`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error under its Playwright name, such as `TargetClosedError`, when
  /// reading the title fails.
  #[qjs(rename = "title")]
  pub async fn title(&self, call_site: crate::bindings::CallSite, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<String> {
    call_site
      .scope(async move { self.inner.title().await.into_js_with(&ctx) })
      .await
  }

  /// Playwright's `frame.content(options?)`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options.includeShadow` is not a boolean, and with the core
  /// error under its Playwright name, such as `TargetClosedError`, when reading the document fails.
  #[qjs(rename = "content")]
  pub async fn content<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    let include_shadow = match options.0.as_ref().and_then(rquickjs::Value::as_object) {
      Some(bag) => bag.get::<_, Option<bool>>("includeShadow")?,
      None => None,
    };
    let options = ferridriver::options::ContentOptions { include_shadow };
    call_site
      .scope(async move { self.inner.content().options(options).await.into_js_with(&ctx) })
      .await
  }

  /// Playwright: `frame.goto(url, options?: { referer?, timeout?,
  /// waitUntil? }): Promise<Response | null>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the navigation fails.
  #[qjs(rename = "goto")]
  pub async fn goto<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    url: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>> {
    call_site
      .scope(async move {
        let opts = crate::bindings::page::parse_goto_options(&ctx, options)?;
        let response = self.inner.goto(&url).maybe_options(opts).await.into_js_with(&ctx)?;
        Ok(response.map(|r| {
          crate::bindings::network::ResponseJs::new_with_page(r, std::sync::Arc::clone(self.inner.page_arc()))
        }))
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Playwright: `frame.setContent(html, options?: { timeout?, waitUntil? })`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when setting the content fails.
  #[qjs(rename = "setContent")]
  pub async fn set_content<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    html: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts: Option<ferridriver::options::SetContentOptions> =
          crate::bindings::convert::parse_opt_bag(&ctx, options)?;
        self
          .inner
          .set_content(&html)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright: `frame.addScriptTag(options?: { content?, path?, type?,
  /// url? }): Promise<ElementHandle>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, a `PermissionDeniedError` when the
  /// realm may not read `options.path`, and with the core error under its Playwright name, such as
  /// `TimeoutError`, when adding the tag fails.
  #[qjs(rename = "addScriptTag")]
  pub async fn add_script_tag<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::element_handle::ElementHandleJs> {
    call_site
      .scope(async move {
        let opts: Option<ferridriver::options::AddScriptTagOptions> =
          crate::bindings::convert::parse_opt_bag(&ctx, options)?;
        if let Some(path) = opts.as_ref().and_then(|o| o.path.as_deref()) {
          crate::bindings::convert::readable(&ctx, path)?;
        }
        let handle = self
          .inner
          .add_script_tag()
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)?;
        Ok(crate::bindings::element_handle::ElementHandleJs::new(handle))
      })
      .await
  }

  /// Playwright: `frame.addStyleTag(options?: { content?, path?, url? }):
  /// Promise<ElementHandle>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, a `PermissionDeniedError` when the
  /// realm may not read `options.path`, and with the core error under its Playwright name, such as
  /// `TimeoutError`, when adding the tag fails.
  #[qjs(rename = "addStyleTag")]
  pub async fn add_style_tag<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::element_handle::ElementHandleJs> {
    call_site
      .scope(async move {
        let opts: Option<ferridriver::options::AddStyleTagOptions> =
          crate::bindings::convert::parse_opt_bag(&ctx, options)?;
        if let Some(path) = opts.as_ref().and_then(|o| o.path.as_deref()) {
          crate::bindings::convert::readable(&ctx, path)?;
        }
        let handle = self
          .inner
          .add_style_tag()
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)?;
        Ok(crate::bindings::element_handle::ElementHandleJs::new(handle))
      })
      .await
  }

  /// Playwright: `frame.$(selector, options?: { strict?: boolean }):
  /// Promise<ElementHandle | null>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TargetClosedError`, when the query fails.
  #[qjs(rename = "$")]
  pub async fn dollar<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::element_handle::ElementHandleJs>> {
    call_site
      .scope(async move {
        let strict = crate::bindings::page::opt_strict(&options)?;
        let handle = self.inner.query_selector(&selector, strict).await.into_js_with(&ctx)?;
        Ok(handle.map(crate::bindings::element_handle::ElementHandleJs::new))
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Playwright: `frame.$$(selector): Promise<ElementHandle[]>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error under its Playwright name, such as `TargetClosedError`, when the
  /// query fails.
  #[qjs(rename = "$$")]
  pub async fn dollar_dollar(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<Vec<crate::bindings::element_handle::ElementHandleJs>> {
    call_site
      .scope(async move {
        let handles = self.inner.query_selector_all(&selector).await.into_js_with(&ctx)?;
        Ok(
          handles
            .into_iter()
            .map(crate::bindings::element_handle::ElementHandleJs::new)
            .collect(),
        )
      })
      .await
  }

  /// Playwright: `frame.frameElement(): Promise<ElementHandle>`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error under its Playwright name, such as `TargetClosedError`, when
  /// resolving the element fails.
  #[qjs(rename = "frameElement")]
  pub async fn frame_element(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<crate::bindings::element_handle::ElementHandleJs> {
    call_site
      .scope(async move {
        let handle = self.inner.frame_element().await.into_js_with(&ctx)?;
        Ok(crate::bindings::element_handle::ElementHandleJs::new(handle))
      })
      .await
  }

  /// Playwright: `frame.waitForLoadState(state?, options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the wait fails.
  #[qjs(rename = "waitForLoadState")]
  pub async fn wait_for_load_state<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    state: Opt<Option<String>>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts: Option<ferridriver::options::WaitForLoadStateOptions> =
          crate::bindings::convert::parse_opt_bag(&ctx, options)?;
        self
          .inner
          .wait_for_load_state(state.0.flatten().as_deref())
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright: `frame.waitForURL(url: string | RegExp | ((url: URL) =>
  /// boolean), options?: { timeout?, waitUntil? })`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, a `TypeError` when `url` is not a
  /// string, `RegExp` or function, what the predicate throws, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the wait fails.
  #[qjs(rename = "waitForURL")]
  pub async fn wait_for_url<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    url: rquickjs::Value<'js>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts: Option<ferridriver::options::WaitForUrlOptions> =
          crate::bindings::convert::parse_opt_bag(&ctx, options)?;
        crate::bindings::page::wait_for_url_condition(&ctx, &url, |condition| {
          self.inner.wait_for_url(condition).maybe_options(opts).into_future()
        })
        .await
      })
      .await
  }

  /// Playwright: `frame.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`.
  #[qjs(rename = "waitForNavigation")]
  #[must_use]
  pub fn wait_for_navigation<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::promise::Promised<
    impl std::future::Future<
      Output = rquickjs::Result<crate::bindings::convert::Null<crate::bindings::network::ResponseJs>>,
    > + 'js,
  > {
    // Listening from the call: the navigation is usually started before
    // the promise is first awaited.
    let navigation = self.inner.wait_for_navigation();
    let opts = crate::bindings::convert::arm_now(&ctx, || {
      crate::bindings::page::parse_wait_for_navigation_options(&ctx, &options)
    });
    let page = std::sync::Arc::clone(self.inner.page_arc());
    rquickjs::promise::Promised::from(call_site.scope(async move {
      let opts = opts.map_err(|deferred| deferred.raise(&ctx))?;
      let response = navigation.options(opts).await.into_js_with(&ctx)?;
      Ok(crate::bindings::convert::Null(
        response.map(|r| crate::bindings::network::ResponseJs::new_with_page(r, page)),
      ))
    }))
  }

  /// Playwright: `frame.waitForFunction(pageFunction, arg?, options?: {
  /// polling?, timeout? }): Promise<JSHandle>`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `arg` is cyclic, nested too deeply or out of range, an `Error`
  /// when `arg` holds a function, a `TypeError` for a malformed `options` bag, and with the core
  /// error when the page function throws or the wait times out.
  #[qjs(rename = "waitForFunction")]
  pub async fn wait_for_function<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    page_function: rquickjs::Value<'js>,
    arg: Opt<rquickjs::Value<'js>>,
    options: Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::js_handle::JSHandleJs> {
    call_site
      .scope(async move {
        let opts: Option<ferridriver::options::WaitForFunctionOptions> =
          crate::bindings::convert::parse_opt_bag(&ctx, options)?;
        let (source, is_function) = extract_page_function(&ctx, page_function)?;
        let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
        let handle = self
          .inner
          .wait_for_function_serialized(&source, serialized, is_function, opts)
          .await
          .into_js_with(&ctx)?;
        Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
      })
      .await
  }

  /// Playwright: `frame.waitForTimeout(timeout): Promise<void>`.
  #[qjs(rename = "waitForTimeout")]
  pub async fn wait_for_timeout(&self, call_site: crate::bindings::CallSite, timeout: f64) {
    call_site
      .scope(async move {
        self
          .inner
          .wait_for_timeout(crate::bindings::convert::f64_to_u64(timeout))
          .await;
      })
      .await;
  }

  /// Playwright: `frame.waitForSelector(selector, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:217`).
  /// Options: `{ state?: 'visible'|'hidden'|'attached'|'detached', timeout?: ms }`.
  /// Resolves to the matched element handle for `state: 'attached'` /
  /// `'visible'` (default), or `null` for `state: 'hidden'` / `'detached'`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the wait fails.
  #[qjs(rename = "waitForSelector")]
  pub async fn wait_for_selector<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<crate::bindings::element_handle::ElementHandleJs>> {
    call_site
      .scope(async move {
        let opts = crate::bindings::page::parse_wait_options(&ctx, options)?;
        let handle = self
          .inner
          .wait_for_selector(&selector)
          .options(opts)
          .await
          .into_js_with(&ctx)?;
        Ok(handle.map(crate::bindings::element_handle::ElementHandleJs::new))
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  // ── Locator (frame-scoped) ─────────────────────────────────────────

  /// Create a locator scoped to this frame.
  #[qjs(rename = "locator")]
  #[must_use]
  pub fn locator(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    selector: String,
  ) -> LocatorJs {
    LocatorJs::new(self.inner.locator(&selector))
  }

  /// Playwright's `frame.getByRole(role, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `options.name` or `options.description` is neither a string nor a
  /// `RegExp`.
  #[qjs(rename = "getByRole")]
  pub fn get_by_role(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    role: String,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let opts = crate::bindings::page::parse_role_options(options)?;
    Ok(LocatorJs::new(
      self.inner.get_by_role(role.as_str()).options(opts).into_locator(),
    ))
  }

  /// Playwright's `frame.getByText(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
  #[qjs(rename = "getByText")]
  pub fn get_by_text(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(&text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_text(t).options(opts).into_locator()))
  }

  /// Playwright's `frame.getByLabel(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
  #[qjs(rename = "getByLabel")]
  pub fn get_by_label(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(&text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_label(t).options(opts).into_locator()))
  }

  /// Playwright's `frame.getByPlaceholder(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
  #[qjs(rename = "getByPlaceholder")]
  pub fn get_by_placeholder(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(&text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(
      self.inner.get_by_placeholder(t).options(opts).into_locator(),
    ))
  }

  /// Playwright's `frame.getByAltText(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
  #[qjs(rename = "getByAltText")]
  pub fn get_by_alt_text(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(&text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(
      self.inner.get_by_alt_text(t).options(opts).into_locator(),
    ))
  }

  /// Playwright's `frame.getByTitle(text, options?)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
  #[qjs(rename = "getByTitle")]
  pub fn get_by_title(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    text: rquickjs::Value<'_>,
    options: rquickjs::function::Opt<rquickjs::Value<'_>>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(&text)?;
    let opts = crate::bindings::page::parse_text_options(options);
    Ok(LocatorJs::new(self.inner.get_by_title(t).options(opts).into_locator()))
  }

  /// Playwright's `frame.getByTestId(testId)`.
  ///
  /// # Errors
  ///
  /// Throws a `TypeError` when `testId` is neither a string nor a `RegExp`.
  #[qjs(rename = "getByTestId")]
  pub fn get_by_test_id(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    test_id: rquickjs::Value<'_>,
  ) -> rquickjs::Result<LocatorJs> {
    let t = crate::bindings::page::string_or_regex_from_js(&test_id)?;
    Ok(LocatorJs::new(self.inner.get_by_test_id(t)))
  }

  /// Playwright: `frame.frameLocator(selector?: string): FrameLocator`.
  #[qjs(rename = "frameLocator")]
  #[must_use]
  pub fn frame_locator(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    selector: rquickjs::function::Opt<String>,
  ) -> crate::bindings::frame_locator::FrameLocatorJs {
    crate::bindings::frame_locator::FrameLocatorJs::new(self.inner.frame_locator(selector.0.as_deref()))
  }

  /// Playwright: `frame.page(): Page`. Returns the owning page; it
  /// carries the session's `AsyncContext` (via userdata) so
  /// `page.route` / `page.exposeFunction` work on the returned handle.
  ///
  /// # Errors
  ///
  /// Throws when the engine cannot create or restore the `Page` object.
  #[qjs(rename = "page")]
  pub fn page<'js>(
    &self,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs hands a method its `Ctx` by value"
    )]
    ctx: rquickjs::Ctx<'js>,
  ) -> rquickjs::Result<rquickjs::Class<'js, crate::bindings::page::PageJs>> {
    crate::bindings::identity::page_object(&ctx, std::sync::Arc::clone(self.inner.page_arc()))
  }

  // ── Action methods (Playwright parity — task 3.9) ──────────────────
  //
  // Mirror the surface from
  // `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447`.
  // Each delegates to the Rust core's `Frame::<action>`, which scopes
  // to this frame's execution context via `Frame::locator`.

  /// Playwright's `frame.click`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the click fails.
  #[qjs(rename = "click")]
  pub async fn click<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_click_options(&ctx, options)?;
        self.inner.click(&selector).maybe_options(opts).await.into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.dblclick`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the double click fails.
  #[qjs(rename = "dblclick")]
  pub async fn dblclick<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
        self
          .inner
          .dblclick(&selector)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.hover`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the hover fails.
  #[qjs(rename = "hover")]
  pub async fn hover<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_hover_options(&ctx, options)?;
        self.inner.hover(&selector).maybe_options(opts).await.into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.tap`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the tap fails.
  #[qjs(rename = "tap")]
  pub async fn tap<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_tap_options(&ctx, options)?;
        self.inner.tap(&selector).maybe_options(opts).await.into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.focus`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error under its Playwright name, such as `TimeoutError`, when focusing
  /// fails.
  #[qjs(rename = "focus")]
  pub async fn focus(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    selector: String,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.focus(&selector).await.into_js_with(&ctx) })
      .await
  }

  /// Playwright's `frame.fill`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the fill fails.
  #[qjs(rename = "fill")]
  pub async fn fill<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    value: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_fill_options(&ctx, options)?;
        self
          .inner
          .fill(&selector, &value)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.type`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when typing fails.
  #[qjs(rename = "type")]
  pub async fn type_text<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    text: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
        self
          .inner
          .r#type(&selector, &text)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.press`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the key press fails.
  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    key: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_press_options(&ctx, options)?;
        self
          .inner
          .press(&selector, &key)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.check`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when checking fails.
  #[qjs(rename = "check")]
  pub async fn check<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
        self.inner.check(&selector).maybe_options(opts).await.into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.uncheck`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when unchecking fails.
  #[qjs(rename = "uncheck")]
  pub async fn uncheck<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
        self
          .inner
          .uncheck(&selector)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.setChecked`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the update fails.
  #[qjs(rename = "setChecked")]
  pub async fn set_checked<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    checked: bool,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let opts = crate::bindings::convert::parse_check_options(&ctx, options)?;
        self
          .inner
          .set_checked(&selector, checked)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.selectOption`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for malformed `values`, a `TypeError` for a malformed `options`
  /// bag, and with the core error under its Playwright name, such as `TimeoutError`, when the
  /// selection fails.
  #[qjs(rename = "selectOption")]
  pub async fn select_option<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    values: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<Vec<String>> {
    call_site
      .scope(async move {
        let values = crate::bindings::convert::parse_select_option_values(&ctx, values)?;
        let opts = crate::bindings::convert::parse_select_option_options(&ctx, options)?;
        self
          .inner
          .select_option(&selector, values)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.setInputFiles`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for malformed `files`, a `PermissionDeniedError` when the realm may
  /// not read a named file, a `TypeError` for a malformed `options` bag, and with the core error
  /// under its Playwright name, such as `TimeoutError`, when setting the files fails.
  #[qjs(rename = "setInputFiles")]
  pub async fn set_input_files<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    files: rquickjs::Value<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let files = crate::bindings::convert::parse_input_files(&ctx, files)?;
        let opts = crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
        self
          .inner
          .set_input_files(&selector, files)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Drag from `source` to `target` selectors within this frame.
  /// Options ride on Locator's drag option bag.
  ///
  /// # Errors
  ///
  /// Rejects with the core error under its Playwright name, such as `TimeoutError`, when the drag
  /// fails.
  #[qjs(rename = "dragAndDrop")]
  pub async fn drag_and_drop(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    source: String,
    target: String,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.drag_and_drop(&source, &target).await.into_js_with(&ctx) })
      .await
  }

  /// Playwright's `frame.dispatchEvent`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `eventInit` cannot be serialized, a `TypeError` for a
  /// malformed `options` bag, and with the core error under its Playwright name, such as
  /// `TimeoutError`, when dispatching the event fails.
  #[qjs(rename = "dispatchEvent")]
  pub async fn dispatch_event<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    event_type: String,
    event_init: rquickjs::function::Opt<rquickjs::Value<'js>>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let init_json = crate::bindings::convert::quickjs_arg_to_serialized(&ctx, event_init.0)?;
        let opts = crate::bindings::convert::parse_dispatch_event_options(&ctx, options)?;
        self
          .inner
          .dispatch_event(&selector, &event_type, init_json)
          .maybe_options(opts)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.textContent`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when reading `textContent` fails.
  #[qjs(rename = "textContent")]
  pub async fn text_content<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<String>> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .text_content(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Playwright's `frame.innerText`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when reading `innerText` fails.
  #[qjs(rename = "innerText")]
  pub async fn inner_text<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .inner_text(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.innerHTML`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when reading `innerHTML` fails.
  #[qjs(rename = "innerHTML")]
  pub async fn inner_html<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .inner_html(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.getAttribute`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when reading the attribute fails.
  #[qjs(rename = "getAttribute")]
  pub async fn get_attribute<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    name: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<crate::bindings::convert::Null<String>> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .get_attribute(&selector, &name)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
      .map(crate::bindings::convert::Null)
  }

  /// Playwright's `frame.inputValue`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when reading the value fails.
  #[qjs(rename = "inputValue")]
  pub async fn input_value<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<String> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .input_value(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.isVisible`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the check fails.
  #[qjs(rename = "isVisible")]
  pub async fn is_visible<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<bool> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .is_visible(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.isHidden`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the check fails.
  #[qjs(rename = "isHidden")]
  pub async fn is_hidden<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<bool> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .is_hidden(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.isEnabled`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the check fails.
  #[qjs(rename = "isEnabled")]
  pub async fn is_enabled<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<bool> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .is_enabled(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.isDisabled`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the check fails.
  #[qjs(rename = "isDisabled")]
  pub async fn is_disabled<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<bool> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .is_disabled(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.isEditable`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the check fails.
  #[qjs(rename = "isEditable")]
  pub async fn is_editable<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<bool> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .is_editable(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Playwright's `frame.isChecked`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, and with the core error under its
  /// Playwright name, such as `TimeoutError`, when the check fails.
  #[qjs(rename = "isChecked")]
  pub async fn is_checked<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    selector: String,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<bool> {
    call_site
      .scope(async move {
        let options = crate::bindings::page::options::selector_read_options(&options)?;
        self
          .inner
          .is_checked(&selector)
          .options(options)
          .await
          .into_js_with(&ctx)
      })
      .await
  }
}
