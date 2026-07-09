//! Frame API -- mirrors Playwright's Frame interface.
//!
//! A Frame represents an execution context within a Page.
//! The main frame is the top-level page frame. Child frames
//! correspond to `<iframe>` elements.
//!
//! Frame has the same evaluation and locator methods as Page,
//! but scoped to its specific frame context.

use std::sync::Arc;

use crate::error::Result;
use crate::locator::Locator;
use crate::options::{RoleOptions, StringOrRegex, TextOptions, WaitOptions};
use crate::page::Page;

/// A frame within a page. Mirrors Playwright's
/// [Frame interface](https://playwright.dev/docs/api/class-frame).
///
/// Frame instances are thin handles — the authoritative name/url/parent
/// state lives in `crate::frame_cache::FrameCache` on the owning Page.
/// Cloning a Frame is cheap (`Arc<Page>` + `Arc<str>`) and multiple
/// clones see the same live state.
#[derive(Clone)]
pub struct Frame {
  /// The page this frame belongs to (Arc for cheap cloning in locator chains).
  page: Arc<Page>,
  /// Frame ID (from CDP or backend). `Arc<str>` so locator chains are cheap.
  pub(crate) id: Arc<str>,
}

impl Frame {
  /// Create a frame handle pointing at an id present in the page's
  /// frame cache. The cache is the source of truth for name/url/parent.
  pub(crate) fn new(page: Arc<Page>, id: Arc<str>) -> Self {
    Self { page, id }
  }

  /// Backend-issued frame identifier. Sync — the underlying `Arc<str>`
  /// is set at construction. Used by the network module to match
  /// `Request.frame()` lookups against the page's frame cache.
  #[must_use]
  pub fn frame_id(&self) -> &str {
    &self.id
  }

  /// Frame name (from the `name` attribute of the iframe element).
  /// Playwright: [`frame.name()`](https://playwright.dev/docs/api/class-frame#frame-name)
  /// -- `name(): string` sync, reads cached state.
  #[must_use]
  pub fn name(&self) -> String {
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).map(|r| r.info.name.clone()).unwrap_or_default())
  }

  /// Frame URL.
  /// Playwright: [`frame.url()`](https://playwright.dev/docs/api/class-frame#frame-url)
  /// -- `url(): string` sync.
  #[must_use]
  pub fn url(&self) -> String {
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).map(|r| r.info.url.clone()).unwrap_or_default())
  }

  /// Whether this is the main (top-level) frame. Mirrors Playwright's
  /// equivalent of `frame.parentFrame() === null`.
  #[must_use]
  pub fn is_main_frame(&self) -> bool {
    self
      .page
      .with_frame_cache(|c| c.main_frame_id().as_deref() == Some(&*self.id))
  }

  /// Parent frame. Returns `None` for the main frame. Sync — reads from
  /// the page's frame cache (Playwright:
  /// [`frame.parentFrame()`](https://playwright.dev/docs/api/class-frame#frame-parent-frame)).
  #[must_use]
  pub fn parent_frame(&self) -> Option<Frame> {
    let pid = self.page.with_frame_cache(|c| c.parent_id(&self.id))?;
    Some(Frame::new(Arc::clone(&self.page), pid))
  }

  /// Child frames. Sync — reads from the page's frame cache.
  /// Playwright: [`frame.childFrames()`](https://playwright.dev/docs/api/class-frame#frame-child-frames).
  #[must_use]
  pub fn child_frames(&self) -> Vec<Frame> {
    let ids = self.page.with_frame_cache(|c| c.child_ids(&self.id));
    ids
      .into_iter()
      .map(|id| Frame::new(Arc::clone(&self.page), id))
      .collect()
  }

  // ── Evaluation (frame-scoped) ────────────────────────────────────────

  /// Playwright: `frame.evaluate(pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:196`).
  ///
  /// Runs `fn_source` in this frame's execution context with `arg`
  /// serialised through the isomorphic wire protocol. Main-frame calls
  /// pass `frame_id = None`; child-frame calls thread `self.id()` so
  /// the utility script resolves the target frame's context.
  ///
  /// # Errors
  ///
  /// Returns a [`crate::error::FerriError`] on page-side exception or
  /// protocol failure.
  pub async fn evaluate(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    let frame_id = if self.is_main_frame() { None } else { Some(&*self.id) };
    let empty = matches!(
      arg.value,
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    ) && arg.handles.is_empty();
    let args_slice: &[crate::protocol::SerializedValue] = if empty { &[] } else { std::slice::from_ref(&arg.value) };
    let result = self
      .page
      .inner
      .call_utility_evaluate(fn_source, args_slice, &arg.handles, frame_id, is_function, true)
      .await?;
    match result {
      crate::js_handle::EvaluateResult::Value(v) => Ok(v),
      crate::js_handle::EvaluateResult::Handle(..) => Err(crate::error::FerriError::Evaluation(
        "Frame::evaluate: backend returned handle but returnByValue=true was requested".into(),
      )),
    }
  }

  /// Playwright: `frame.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:190`).
  ///
  /// Same wire path as [`Self::evaluate`] but retains the result on
  /// the page and hands back a fresh [`crate::js_handle::JSHandle`].
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`].
  pub async fn evaluate_handle(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::js_handle::JSHandle> {
    let frame_id = if self.is_main_frame() { None } else { Some(&*self.id) };
    let empty = matches!(
      arg.value,
      crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
    ) && arg.handles.is_empty();
    let args_slice: &[crate::protocol::SerializedValue] = if empty { &[] } else { std::slice::from_ref(&arg.value) };
    let result = self
      .page
      .inner
      .call_utility_evaluate(fn_source, args_slice, &arg.handles, frame_id, is_function, false)
      .await?;
    match result {
      crate::js_handle::EvaluateResult::Handle(backing, is_node) => Ok(crate::js_handle::JSHandle::from_backing(
        Arc::clone(&self.page),
        backing,
        is_node,
      )),
      crate::js_handle::EvaluateResult::Value(_) => Err(crate::error::FerriError::Evaluation(
        "Frame::evaluate_handle: backend returned value but returnByValue=false was requested".into(),
      )),
    }
  }

  /// Typed evaluate: run `fn_source` in this frame and deserialize the
  /// result via serde. Ergonomic wrapper over the wire-level
  /// [`Self::evaluate`] for JSON-shaped values:
  ///
  /// ```ignore
  /// let count: u32 = frame.eval("() => document.images.length").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// See [`Self::evaluate`], plus [`crate::error::FerriError::Json`] /
  /// [`crate::error::FerriError::Evaluation`] when the result does not
  /// decode into `T` (rich JS values need [`Self::evaluate_handle`]).
  pub async fn eval<T: serde::de::DeserializeOwned>(&self, fn_source: &str) -> Result<T> {
    let value = self
      .evaluate(fn_source, crate::protocol::SerializedArgument::default(), None)
      .await?;
    crate::protocol::result_to_serde(&value)
  }

  /// [`Self::eval`] with a serde-serialized argument, passed to the page
  /// function as its single parameter:
  ///
  /// ```ignore
  /// let hit: bool = frame.eval_with("sel => !!document.querySelector(sel)", &"#app").await?;
  /// ```
  ///
  /// # Errors
  ///
  /// See [`Self::eval`].
  pub async fn eval_with<T: serde::de::DeserializeOwned>(
    &self,
    fn_source: &str,
    arg: &(impl serde::Serialize + ?Sized),
  ) -> Result<T> {
    let arg = crate::protocol::argument_from_serde(arg)?;
    let value = self.evaluate(fn_source, arg, None).await?;
    crate::protocol::result_to_serde(&value)
  }

  /// Backend-level expression evaluation used by the frame's internal
  /// plumbing — dispatches to the right backend method (`evaluate` vs
  /// `evaluate_in_frame`) depending on whether this is the main frame.
  /// Not part of the public Playwright API; public callers use
  /// [`Self::evaluate`] with a function literal.
  async fn backend_eval_expr(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    if self.is_main_frame() {
      self.page.inner.evaluate(expression).await
    } else {
      self.page.inner.evaluate_in_frame(expression, &self.id).await
    }
  }

  // ── Locators (frame-scoped) ──────────────────────────────────────────

  /// Create a locator scoped to this frame.
  ///
  /// Playwright: `frame.locator(selector, options?: LocatorOptions): Locator`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:324`).
  /// The options-bag form is [`Self::locator_with`].
  #[must_use]
  pub fn locator(&self, selector: &str) -> Locator {
    Locator::new(self.clone(), selector.to_string())
  }

  /// [`Self::locator`] with Playwright's `LocatorOptions` filter bag
  /// (including `visible`).
  #[must_use]
  pub fn locator_with(&self, selector: &str, options: &crate::options::FilterOptions) -> Locator {
    self.locator(selector).filter(options)
  }

  #[must_use]
  pub fn get_by_role(
    &self,
    role: impl Into<crate::options::Role>,
  ) -> crate::locator_builder::LocatorBuilder<RoleOptions> {
    let frame = self.clone();
    let role = role.into();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(frame.clone(), crate::locator::build_role_selector(role.as_str(), opts))
    })
  }

  /// `getByText` in this frame. Accepts `string | RegExp`.
  #[must_use]
  pub fn get_by_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:text", text.into())
  }

  /// `getByTestId` in this frame.
  #[must_use]
  pub fn get_by_test_id(&self, test_id: impl Into<StringOrRegex>) -> Locator {
    Locator::new(
      self.clone(),
      crate::locator::build_testid_selector("data-testid", &test_id.into()),
    )
  }

  /// `getByLabel` in this frame.
  #[must_use]
  pub fn get_by_label(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.text_like_builder("internal:label", text.into())
  }

  /// `getByPlaceholder` in this frame.
  #[must_use]
  pub fn get_by_placeholder(
    &self,
    text: impl Into<StringOrRegex>,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("placeholder", text.into())
  }

  /// Locate elements by `alt` attribute. Mirrors Playwright's
  /// `frame.getByAltText(text, options?)`.
  #[must_use]
  pub fn get_by_alt_text(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("alt", text.into())
  }

  /// Locate elements by `title` attribute. Mirrors Playwright's
  /// `frame.getByTitle(text, options?)`.
  #[must_use]
  pub fn get_by_title(&self, text: impl Into<StringOrRegex>) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    self.attr_builder("title", text.into())
  }

  fn text_like_builder(
    &self,
    kind: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let frame = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(
        frame.clone(),
        crate::locator::build_text_like_selector(kind, &text, opts),
      )
    })
  }

  fn attr_builder(
    &self,
    attr: &'static str,
    text: StringOrRegex,
  ) -> crate::locator_builder::LocatorBuilder<TextOptions> {
    let frame = self.clone();
    crate::locator_builder::LocatorBuilder::new(move |opts| {
      Locator::new(frame.clone(), crate::locator::build_attr_selector(attr, &text, opts))
    })
  }

  /// Create a `FrameLocator` for an `<iframe>` matching `selector`
  /// inside this frame's document. Mirrors Playwright's
  /// `frame.frameLocator(selector)`.
  #[must_use]
  pub fn frame_locator(&self, selector: &str) -> crate::locator::FrameLocator {
    crate::locator::FrameLocator::for_iframe_in(self.clone(), selector.to_string())
  }

  // ── Content (frame-scoped) ───────────────────────────────────────────

  /// Get the frame's HTML content.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn content(&self) -> Result<String> {
    let r = self.backend_eval_expr("document.documentElement.outerHTML").await?;
    Ok(
      r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  }

  /// Get the frame's title.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn title(&self) -> Result<String> {
    let r = self.backend_eval_expr("document.title").await?;
    Ok(
      r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  }

  // ── Navigation (frame-scoped) ────────────────────────────────────────

  /// Navigate this frame to a URL.
  ///
  /// # Errors
  ///
  /// Returns an error if navigation fails.
  pub async fn goto(&self, url: &str) -> Result<Option<crate::network::Response>> {
    if self.is_main_frame() {
      self.page.goto_impl(url, None).await
    } else {
      // For child frames, set location via JS
      self
        .backend_eval_expr(&format!("window.location.href = '{}'", url.replace('\'', "\\'")))
        .await?;
      Ok(None)
    }
  }

  // ── Waiting ──────────────────────────────────────────────────────────

  /// Wait for a selector within this frame and return the matched handle.
  ///
  /// Playwright: `frame.waitForSelector(selector, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:217`).
  /// For the default `state: 'visible'` and for `state: 'attached'` the
  /// resolved [`crate::element_handle::ElementHandle`] is returned. For
  /// `state: 'hidden' | 'detached'` the element is gone (or invisible)
  /// so Playwright returns `null`; we mirror that with `Ok(None)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element does not reach the requested state
  /// within the timeout.
  pub fn wait_for_selector(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, WaitOptions, Option<crate::element_handle::ElementHandle>> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.wait_for_selector_impl(&selector, opts).await }))
  }

  pub(crate) async fn wait_for_selector_impl(
    &self,
    selector: &str,
    opts: WaitOptions,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    let state = opts.state;
    let locator = self.locator(selector);
    locator.wait_for_impl(opts).await?;
    // Playwright returns a handle only when the element is present; the
    // `hidden` / `detached` states resolve precisely because it is not.
    let returns_handle = !matches!(
      state,
      Some(crate::options::WaitState::Hidden | crate::options::WaitState::Detached)
    );
    if returns_handle {
      Ok(Some(locator.element_handle().await?))
    } else {
      Ok(None)
    }
  }

  /// Show the element-highlight overlay for `selector` in this frame.
  /// `style` is an optional resolved CSS string applied to the highlight
  /// box. Internal API mirroring Playwright's
  /// `frame._highlight(selector, style)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:338`).
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing or JS evaluation fails.
  pub async fn highlight(&self, selector: &str, style: Option<&str>) -> Result<()> {
    let frame_id = if self.is_main_frame() { None } else { Some(&*self.id) };
    crate::selectors::highlight(self.page.inner(), selector, style, frame_id).await
  }

  /// Hide the element-highlight overlay in this frame. Internal API
  /// mirroring Playwright's `frame._hideHighlight(selector)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:342`).
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn hide_highlight(&self) -> Result<()> {
    let frame_id = if self.is_main_frame() { None } else { Some(&*self.id) };
    crate::selectors::hide_highlight(self.page.inner(), frame_id).await
  }

  /// Whether this frame has been detached from the page. Sync -- reads
  /// the cached `detached` flag maintained by the page's frame event
  /// listener. Playwright:
  /// [`frame.isDetached()`](https://playwright.dev/docs/api/class-frame#frame-is-detached).
  #[must_use]
  pub fn is_detached(&self) -> bool {
    self
      .page
      .with_frame_cache(|c| c.record(&self.id).is_none_or(|r| r.detached))
  }

  /// Get the page this frame belongs to.
  #[must_use]
  pub fn page(&self) -> &Page {
    &self.page
  }

  /// Reference to the owning `Arc<Page>`. Locators hold a `Frame` and
  /// reach the backend through `frame.page_arc()`.
  #[must_use]
  pub fn page_arc(&self) -> &Arc<Page> {
    &self.page
  }

  /// Backend frame id (CDP/BiDi). Stable through navigations; used to
  /// scope evaluation to this frame's execution context.
  #[must_use]
  pub fn id(&self) -> &Arc<str> {
    &self.id
  }

  /// Set the HTML content of this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    let escaped = crate::steps::js_escape(html);
    self
      .backend_eval_expr(&format!("document.documentElement.innerHTML = '{escaped}'"))
      .await?;
    Ok(())
  }

  /// Add a `<script>` tag to this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if script injection fails.
  pub async fn add_script_tag(
    &self,
    url: Option<&str>,
    content: Option<&str>,
    script_type: Option<&str>,
  ) -> Result<()> {
    let t = script_type.unwrap_or("text/javascript");
    if let Some(url) = url {
      self.backend_eval_expr(&format!(
                "(function(){{return new Promise(function(r,j){{var s=document.createElement('script');\
                 s.type='{}';s.src='{}';s.onload=r;s.onerror=function(){{j(new Error('Failed'))}};document.head.appendChild(s)}})}})();",
                crate::steps::js_escape(t), crate::steps::js_escape(url)
            )).await?;
    } else if let Some(content) = content {
      self.backend_eval_expr(&format!(
                "(function(){{var s=document.createElement('script');s.type='{}';s.text='{}';document.head.appendChild(s)}})()",
                crate::steps::js_escape(t), crate::steps::js_escape(content)
            )).await?;
    }
    Ok(())
  }

  /// Add a `<style>` tag or `<link>` stylesheet to this frame.
  ///
  /// # Errors
  ///
  /// Returns an error if style injection fails.
  pub async fn add_style_tag(&self, url: Option<&str>, content: Option<&str>) -> Result<()> {
    if let Some(url) = url {
      self.backend_eval_expr(&format!(
                "(function(){{return new Promise(function(r,j){{var l=document.createElement('link');\
                 l.rel='stylesheet';l.href='{}';l.onload=r;l.onerror=function(){{j(new Error('Failed'))}};document.head.appendChild(l)}})}})();",
                crate::steps::js_escape(url)
            )).await?;
    } else if let Some(content) = content {
      self
        .backend_eval_expr(&format!(
          "(function(){{var s=document.createElement('style');s.textContent='{}';document.head.appendChild(s)}})()",
          crate::steps::js_escape(content)
        ))
        .await?;
    }
    Ok(())
  }

  /// Wait for the frame to reach a specific load state.
  ///
  /// # Errors
  ///
  /// Returns an error if the frame does not reach load state within the timeout.
  pub async fn wait_for_load_state(&self) -> Result<()> {
    if self.is_main_frame() {
      self.page.wait_for_load_state(None).await
    } else {
      // For iframes, check document.readyState via JS
      let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
      loop {
        if tokio::time::Instant::now() >= deadline {
          return Err(crate::error::FerriError::timeout(
            "waiting for frame load state",
            30_000,
          ));
        }
        if let Ok(Some(v)) = self.backend_eval_expr("document.readyState").await {
          if v.as_str() == Some("complete") {
            return Ok(());
          }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
      }
    }
  }

  // ── Action methods (Playwright parity — task 3.9) ──────────────────────
  //
  // Mirrors Playwright's frame action surface from
  // `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447`.
  // Each method delegates to `self.locator(selector).<action>()` —
  // Frame's locator already scopes by `frame_id`, so the action runs in
  // the iframe's execution context (CDP) or against the synthesized
  // iframe (WebKit). Option bags are intentionally minimal here; they
  // ride on top of the existing Locator surface and pick up extensions
  // (timeout/force/etc.) when those land on Locator itself.

  // -- Mouse / pointer ---------------------------------------------------

  /// Click the element matched by `selector`. Accepts Playwright's full
  /// `FrameClickOptions` bag (see [`crate::options::ClickOptions`]).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the click fails.
  pub fn click(&self, selector: &str) -> crate::action::Action<'static, crate::options::ClickOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.click_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn click_impl(&self, selector: &str, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    self.locator(selector).click_impl(opts).await
  }

  /// Double-click the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the dblclick fails.
  pub fn dblclick(&self, selector: &str) -> crate::action::Action<'static, crate::options::DblClickOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.dblclick_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn dblclick_impl(
    &self,
    selector: &str,
    opts: Option<crate::options::DblClickOptions>,
  ) -> Result<()> {
    self.locator(selector).dblclick_impl(opts).await
  }

  /// Hover the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the hover fails.
  pub fn hover(&self, selector: &str) -> crate::action::Action<'static, crate::options::HoverOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.hover_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn hover_impl(&self, selector: &str, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    self.locator(selector).hover_impl(opts).await
  }

  /// Tap (touch) the element matched by `selector`. Mirrors
  /// `frame.tap(selector, options?)` per
  /// `/tmp/playwright/packages/playwright-core/src/client/frame.ts:308`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the tap fails.
  pub fn tap(&self, selector: &str) -> crate::action::Action<'static, crate::options::TapOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.tap_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn tap_impl(&self, selector: &str, opts: Option<crate::options::TapOptions>) -> Result<()> {
    self.locator(selector).tap_impl(opts).await
  }

  /// Focus the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or focus fails.
  pub async fn focus(&self, selector: &str) -> Result<()> {
    self.locator(selector).focus().await
  }

  // -- Form input --------------------------------------------------------

  /// Fill an input matching `selector` with `value`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not fillable.
  pub fn fill(&self, selector: &str, value: &str) -> crate::action::Action<'static, crate::options::FillOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let value = value.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.fill_impl(&selector, &value, Some(opts)).await })
    })
  }

  pub(crate) async fn fill_impl(
    &self,
    selector: &str,
    value: &str,
    opts: Option<crate::options::FillOptions>,
  ) -> Result<()> {
    self.locator(selector).fill_impl(value, opts).await
  }

  /// Type characters into an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or typing fails.
  pub fn r#type(&self, selector: &str, text: &str) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let text = text.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.type_impl(&selector, &text, Some(opts)).await }))
  }

  pub(crate) async fn type_impl(
    &self,
    selector: &str,
    text: &str,
    opts: Option<crate::options::TypeOptions>,
  ) -> Result<()> {
    self.locator(selector).type_impl(text, opts).await
  }

  /// Press a key on an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the key press fails.
  pub fn press(&self, selector: &str, key: &str) -> crate::action::Action<'static, crate::options::PressOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let key = key.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.press_impl(&selector, &key, Some(opts)).await }))
  }

  pub(crate) async fn press_impl(
    &self,
    selector: &str,
    key: &str,
    opts: Option<crate::options::PressOptions>,
  ) -> Result<()> {
    self.locator(selector).press_impl(key, opts).await
  }

  /// Check a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  pub fn check(&self, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.check_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn check_impl(&self, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.locator(selector).check_impl(opts).await
  }

  /// Uncheck a checkbox matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not uncheckable.
  pub fn uncheck(&self, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.uncheck_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn uncheck_impl(&self, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.locator(selector).uncheck_impl(opts).await
  }

  /// Set the checked state of a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  pub fn set_checked(
    &self,
    selector: &str,
    checked: bool,
  ) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.set_checked_impl(&selector, checked, Some(opts)).await })
    })
  }

  pub(crate) async fn set_checked_impl(
    &self,
    selector: &str,
    checked: bool,
    opts: Option<crate::options::CheckOptions>,
  ) -> Result<()> {
    self.locator(selector).set_checked_impl(checked, opts).await
  }

  /// Select a `<select>` option in the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the option cannot
  /// be selected.
  pub fn select_option(
    &self,
    selector: &str,
    values: impl Into<crate::options::SelectOptionValues>,
  ) -> crate::action::Action<'static, crate::options::SelectOptionOptions, Vec<String>> {
    let values = values.into().0;
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.select_option_impl(&selector, values, Some(opts)).await })
    })
  }

  pub(crate) async fn select_option_impl(
    &self,
    selector: &str,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self.locator(selector).select_option_impl(values, opts).await
  }

  /// Set input files on a `<input type=file>` matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or file setting fails.
  pub fn set_input_files(
    &self,
    selector: &str,
    files: impl Into<crate::options::InputFiles>,
  ) -> crate::action::Action<'static, crate::options::SetInputFilesOptions, ()> {
    let files = files.into();
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.set_input_files_impl(&selector, files, Some(opts)).await })
    })
  }

  pub(crate) async fn set_input_files_impl(
    &self,
    selector: &str,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    self.locator(selector).set_input_files_impl(files, opts).await
  }

  // -- Drag and drop -----------------------------------------------------

  /// Drag from `source` to `target` selectors within this frame. Mirrors
  /// `frame.dragAndDrop(source, target, options?)` per
  /// `/tmp/playwright/packages/playwright-core/src/client/frame.ts:304`.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found or the
  /// drag-and-drop operation fails.
  pub fn drag_and_drop(
    &self,
    source: &str,
    target: &str,
  ) -> crate::action::Action<'static, crate::options::DragAndDropOptions, ()> {
    let frame = self.clone();
    let source = source.to_string();
    let target = target.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { frame.drag_and_drop_impl(&source, &target, Some(opts)).await })
    })
  }

  pub(crate) async fn drag_and_drop_impl(
    &self,
    source: &str,
    target: &str,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();
    let src = self.locator(source);
    let tgt = self.locator(target);
    let (src, tgt) = match opts.strict {
      Some(s) => (src.strict(s), tgt.strict(s)),
      None => (src, tgt),
    };
    src.drag_to_impl(&tgt, Some(opts)).await
  }

  // -- Synthetic events --------------------------------------------------

  /// Dispatch a DOM event on the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the dispatch fails.
  pub fn dispatch_event(
    &self,
    selector: &str,
    event_type: &str,
    event_init: Option<serde_json::Value>,
  ) -> crate::action::Action<'static, crate::options::DispatchEventOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    let event_type = event_type.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .dispatch_event_impl(&selector, &event_type, event_init, Some(opts))
          .await
      })
    })
  }

  pub(crate) async fn dispatch_event_impl(
    &self,
    selector: &str,
    event_type: &str,
    event_init: Option<serde_json::Value>,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .locator(selector)
      .dispatch_event_impl(event_type, event_init, opts)
      .await
  }

  // -- Content / attribute reads ----------------------------------------

  /// Get the text content of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn text_content(&self, selector: &str) -> Result<Option<String>> {
    self.locator(selector).text_content().await
  }

  /// Get `innerText` of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_text(&self, selector: &str) -> Result<String> {
    self.locator(selector).inner_text().await
  }

  /// Get `innerHTML` of the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn inner_html(&self, selector: &str) -> Result<String> {
    self.locator(selector).inner_html().await
  }

  /// Get an attribute on the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn get_attribute(&self, selector: &str, name: &str) -> Result<Option<String>> {
    self.locator(selector).get_attribute(name).await
  }

  /// Get `value` from a form control matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn input_value(&self, selector: &str) -> Result<String> {
    self.locator(selector).input_value().await
  }

  // -- State checks ------------------------------------------------------

  /// True if the element matched by `selector` is visible.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_visible(&self, selector: &str) -> Result<bool> {
    self.locator(selector).is_visible().await
  }

  /// True if the element matched by `selector` is hidden.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_hidden(&self, selector: &str) -> Result<bool> {
    self.locator(selector).is_hidden().await
  }

  /// True if the element matched by `selector` is enabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_enabled(&self, selector: &str) -> Result<bool> {
    self.locator(selector).is_enabled().await
  }

  /// True if the element matched by `selector` is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_disabled(&self, selector: &str) -> Result<bool> {
    self.locator(selector).is_disabled().await
  }

  /// True if the element matched by `selector` is editable.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_editable(&self, selector: &str) -> Result<bool> {
    self.locator(selector).is_editable().await
  }

  /// True if a checkbox/radio matched by `selector` is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn is_checked(&self, selector: &str) -> Result<bool> {
    self.locator(selector).is_checked().await
  }
}

impl std::fmt::Debug for Frame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let (name, url, main) = self.page.with_frame_cache(|c| {
      let rec = c.record(&self.id);
      (
        rec.map(|r| r.info.name.clone()),
        rec.map(|r| r.info.url.clone()),
        c.main_frame_id().as_deref() == Some(&*self.id),
      )
    });
    f.debug_struct("Frame")
      .field("id", &self.id)
      .field("name", &name)
      .field("url", &url)
      .field("main", &main)
      .finish_non_exhaustive()
  }
}
