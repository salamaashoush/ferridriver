//! Page actions, state checks, waiting, focus/dispatch, and low-level mouse.

use std::sync::Arc;

use crate::error::Result;
use crate::frame::Frame;
use crate::frame_cache::FrameCache;
use crate::options::{FrameSelector, WaitOptions};

use super::Page;

impl Page {
  // ── Action methods (delegate to mainFrame — Playwright parity) ─────
  //
  // Mirrors `/tmp/playwright/packages/playwright-core/src/client/page.ts:658+`:
  // every action delegates to `this._mainFrame.<method>(...)`. The
  // `tracing::debug!` events stay at this layer so logs identify the
  // top-level entry point.

  /// Click on an element matching the selector. Accepts Playwright's
  /// full `PageClickOptions` bag (see [`crate::options::ClickOptions`]).
  /// Delegates to `mainFrame().click(selector, opts)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the click fails.
  #[track_caller]
  pub fn click(self: &Arc<Self>, selector: &str) -> crate::action::Action<'static, crate::options::ClickOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.click_impl(&selector, Some(opts)).await }))
  }

  /// Implementation of [`Self::click`].
  pub(crate) async fn click_impl(
    self: &Arc<Self>,
    selector: &str,
    opts: Option<crate::options::ClickOptions>,
  ) -> Result<()> {
    tracing::debug!(target: "ferridriver::action", action = "click", selector, "page.click");
    self.main_frame().click_impl(selector, opts).await
  }

  /// Double-click an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the double-click fails.
  #[track_caller]
  pub fn dblclick(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::DblClickOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.dblclick_impl(&selector, Some(opts)).await }))
  }

  /// Implementation of [`Self::dblclick`].
  pub(crate) async fn dblclick_impl(
    self: &Arc<Self>,
    selector: &str,
    opts: Option<crate::options::DblClickOptions>,
  ) -> Result<()> {
    tracing::debug!(target: "ferridriver::action", action = "dblclick", selector, "page.dblclick");
    self.main_frame().dblclick_impl(selector, opts).await
  }

  /// Fill an input element matching the selector with a value.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not fillable.
  #[track_caller]
  pub fn fill(
    self: &Arc<Self>,
    selector: &str,
    value: &str,
  ) -> crate::action::Action<'static, crate::options::FillOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    let value = value.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.fill_impl(&selector, &value, Some(opts)).await }))
  }

  /// Implementation of [`Self::fill`].
  pub(crate) async fn fill_impl(
    self: &Arc<Self>,
    selector: &str,
    value: &str,
    opts: Option<crate::options::FillOptions>,
  ) -> Result<()> {
    tracing::debug!(target: "ferridriver::action", action = "fill", selector, "page.fill");
    self.main_frame().fill_impl(selector, value, opts).await
  }

  /// Type text character-by-character into an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or typing fails.
  #[track_caller]
  pub fn r#type(
    self: &Arc<Self>,
    selector: &str,
    text: &str,
  ) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    let text = text.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.type_impl(&selector, &text, Some(opts)).await }))
  }

  /// Implementation of [`Self::r#type`].
  pub(crate) async fn type_impl(
    self: &Arc<Self>,
    selector: &str,
    text: &str,
    opts: Option<crate::options::TypeOptions>,
  ) -> Result<()> {
    self.main_frame().type_impl(selector, text, opts).await
  }

  /// Press a key on an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the key press fails.
  #[track_caller]
  pub fn press(
    self: &Arc<Self>,
    selector: &str,
    key: &str,
  ) -> crate::action::Action<'static, crate::options::PressOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    let key = key.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.press_impl(&selector, &key, Some(opts)).await }))
  }

  /// Implementation of [`Self::press`].
  pub(crate) async fn press_impl(
    self: &Arc<Self>,
    selector: &str,
    key: &str,
    opts: Option<crate::options::PressOptions>,
  ) -> Result<()> {
    self.main_frame().press_impl(selector, key, opts).await
  }

  /// Hover over an element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the hover fails.
  #[track_caller]
  pub fn hover(self: &Arc<Self>, selector: &str) -> crate::action::Action<'static, crate::options::HoverOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.hover_impl(&selector, Some(opts)).await }))
  }

  /// Implementation of [`Self::hover`].
  pub(crate) async fn hover_impl(
    self: &Arc<Self>,
    selector: &str,
    opts: Option<crate::options::HoverOptions>,
  ) -> Result<()> {
    self.main_frame().hover_impl(selector, opts).await
  }

  /// Select an option in a `<select>` element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the option cannot be selected.
  #[track_caller]
  pub fn select_option(
    self: &Arc<Self>,
    selector: &str,
    values: impl Into<crate::options::SelectOptionValues>,
  ) -> crate::action::Action<'static, crate::options::SelectOptionOptions, Vec<String>> {
    let values = values.into().0;
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { page.select_option_impl(&selector, values, Some(opts)).await })
    })
  }

  /// Implementation of [`Self::select_option`].
  pub(crate) async fn select_option_impl(
    self: &Arc<Self>,
    selector: &str,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self.main_frame().select_option_impl(selector, values, opts).await
  }

  /// Set input files on a file input element matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or file setting fails.
  #[track_caller]
  pub fn set_input_files(
    self: &Arc<Self>,
    selector: &str,
    files: impl Into<crate::options::InputFiles>,
  ) -> crate::action::Action<'static, crate::options::SetInputFilesOptions, ()> {
    let files = files.into();
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { page.set_input_files_impl(&selector, files, Some(opts)).await })
    })
  }

  /// Implementation of [`Self::set_input_files`].
  pub(crate) async fn set_input_files_impl(
    self: &Arc<Self>,
    selector: &str,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    self.main_frame().set_input_files_impl(selector, files, opts).await
  }

  /// Check a checkbox or radio button matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  #[track_caller]
  pub fn check(self: &Arc<Self>, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.check_impl(&selector, Some(opts)).await }))
  }

  /// Implementation of [`Self::check`].
  pub(crate) async fn check_impl(
    self: &Arc<Self>,
    selector: &str,
    opts: Option<crate::options::CheckOptions>,
  ) -> Result<()> {
    self.main_frame().check_impl(selector, opts).await
  }

  /// Uncheck a checkbox matching the selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not uncheckable.
  #[track_caller]
  pub fn uncheck(self: &Arc<Self>, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.uncheck_impl(&selector, Some(opts)).await }))
  }

  /// Implementation of [`Self::uncheck`].
  pub(crate) async fn uncheck_impl(
    self: &Arc<Self>,
    selector: &str,
    opts: Option<crate::options::CheckOptions>,
  ) -> Result<()> {
    self.main_frame().uncheck_impl(selector, opts).await
  }

  /// Set a checkbox or radio matching `selector` to `checked`. Mirrors
  /// Playwright's `page.setChecked(selector, checked, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:439`).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  #[track_caller]
  pub fn set_checked(
    self: &Arc<Self>,
    selector: &str,
    checked: bool,
  ) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { page.set_checked_impl(&selector, checked, Some(opts)).await })
    })
  }

  /// Implementation of [`Self::set_checked`].
  pub(crate) async fn set_checked_impl(
    self: &Arc<Self>,
    selector: &str,
    checked: bool,
    opts: Option<crate::options::CheckOptions>,
  ) -> Result<()> {
    self.main_frame().set_checked_impl(selector, checked, opts).await
  }

  /// Tap (touch) the element matched by `selector`. Mirrors Playwright's
  /// `page.tap(selector, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:308`).
  /// Distinct from `Touchscreen::tap(x, y)` which is the lower-level
  /// coordinate-based touch primitive.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the tap fails.
  #[track_caller]
  pub fn tap(self: &Arc<Self>, selector: &str) -> crate::action::Action<'static, crate::options::TapOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.tap_impl(&selector, Some(opts)).await }))
  }

  /// Implementation of [`Self::tap`].
  pub(crate) async fn tap_impl(
    self: &Arc<Self>,
    selector: &str,
    opts: Option<crate::options::TapOptions>,
  ) -> Result<()> {
    self.main_frame().tap_impl(selector, opts).await
  }

  // ── State checks (delegate to mainFrame) ────────────────────────────

  /// Check if an element matching the selector is visible.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn is_visible(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.main_frame().is_visible(selector)
  }

  /// Check if an element matching the selector is hidden.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn is_hidden(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.main_frame().is_hidden(selector)
  }

  /// Check if an element matching the selector is enabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn is_enabled(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.main_frame().is_enabled(selector)
  }

  /// Check if an element matching the selector is disabled.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn is_disabled(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.main_frame().is_disabled(selector)
  }

  /// Check if a checkbox or radio button matching the selector is checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn is_checked(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.main_frame().is_checked(selector)
  }

  // ── Waiting ─────────────────────────────────────────────────────────────

  /// Wait for an element matching the selector to satisfy the wait
  /// condition, in the main frame. Playwright:
  /// `page.waitForSelector(selector, options?: { state?, strict?, timeout? })`,
  /// resolving to the element for `attached` / `visible` and to `null`
  /// for `hidden` / `detached`. Of several matches the first is taken,
  /// unless `strict`.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out, or on more than one match
  /// under `strict`.
  #[track_caller]
  pub fn wait_for_selector(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, WaitOptions, Option<crate::element_handle::ElementHandle>> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { page.wait_for_selector_impl(&selector, opts).await }))
  }

  /// Implementation of [`Self::wait_for_selector`].
  pub(crate) async fn wait_for_selector_impl(
    self: &Arc<Self>,
    selector: &str,
    opts: WaitOptions,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    self.main_frame().wait_for_selector_impl(selector, opts).await
  }

  /// Wait for the page URL to match the given matcher.
  ///
  /// Accepts a [`crate::url_matcher::UrlMatcher`] (glob, regex or sync
  /// predicate) or a [`crate::url_matcher::UrlCondition`] whose predicate
  /// answers asynchronously.
  /// Playwright: `page.waitForURL(url: string | RegExp | ((url: URL) => boolean),
  /// options?: { timeout?, waitUntil? })` — once the URL matches, the
  /// `waitUntil` lifecycle state (default `load`) is awaited within the
  /// same timeout budget; `commit` returns on the URL match alone.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out or the predicate fails.
  #[track_caller]
  pub fn wait_for_url(
    self: &Arc<Self>,
    url: impl Into<crate::url_matcher::UrlCondition>,
  ) -> crate::action::Action<'static, crate::options::WaitForUrlOptions, ()> {
    let page = Arc::clone(self);
    let condition = url.into();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        page
          .traced(
            "waitForURL",
            serde_json::json!({ "url": condition.describe() }),
            page.wait_for_url_impl(&condition, opts),
          )
          .await
      })
    })
  }

  /// Implementation of [`Self::wait_for_url`], which is the main frame's.
  pub(crate) async fn wait_for_url_impl(
    self: &Arc<Self>,
    condition: &crate::url_matcher::UrlCondition,
    opts: crate::options::WaitForUrlOptions,
  ) -> Result<()> {
    self.main_frame().wait_for_url_impl(condition, opts).await
  }

  pub async fn wait_for_timeout(&self, ms: u64) {
    let _ = self
      .traced("waitForTimeout", serde_json::json!({ "timeout": ms }), async {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Ok(())
      })
      .await;
  }

  /// Wait for a specific load state. Supported states:
  /// - `"load"` (default) - wait for `document.readyState === "complete"`
  /// - `"domcontentloaded"` - wait for `document.readyState !== "loading"`
  /// - `"networkidle"` - wait for no network activity for 500ms.
  ///   Counts `fetch` and `XMLHttpRequest` while they are in flight
  ///   (the injected counter) as well as completed resource timings. A
  ///   resource timing entry only exists once its request has finished,
  ///   so without the counter a pending `fetch` was invisible and this
  ///   returned while an action's request was still running.
  ///
  /// Playwright: `page.waitForLoadState(state?, options?: { timeout? })`.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out before the load state is reached.
  #[track_caller]
  pub fn wait_for_load_state(
    self: &Arc<Self>,
    state: Option<&str>,
  ) -> crate::action::Action<'static, crate::options::WaitForLoadStateOptions, ()> {
    let page = Arc::clone(self);
    let state = state.map(str::to_string);
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        page
          .traced(
            "waitForLoadState",
            serde_json::json!({ "state": state.clone().unwrap_or_else(|| "load".to_string()) }),
            page.wait_for_load_state_impl(state.as_deref(), opts),
          )
          .await
      })
    })
  }

  /// Implementation of [`Self::wait_for_load_state`], which is the main
  /// frame's.
  pub(crate) async fn wait_for_load_state_impl(
    self: &Arc<Self>,
    state: Option<&str>,
    opts: crate::options::WaitForLoadStateOptions,
  ) -> Result<()> {
    self.main_frame().wait_for_load_state_impl(state, opts).await
  }

  // ── Focus / dispatch ─────────────────────────────────────────────────

  /// Focus an element by selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  pub async fn focus(self: &Arc<Self>, selector: &str) -> Result<()> {
    self.locator(selector).focus().await
  }

  /// Dispatch an event on an element by selector.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the event dispatch fails.
  #[track_caller]
  pub fn dispatch_event(
    self: &Arc<Self>,
    selector: &str,
    event_type: &str,
    event_init: crate::protocol::SerializedArgument,
  ) -> crate::action::Action<'static, crate::options::DispatchEventOptions, ()> {
    let page = Arc::clone(self);
    let selector = selector.to_string();
    let event_type = event_type.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        page
          .dispatch_event_impl(&selector, &event_type, event_init, Some(opts))
          .await
      })
    })
  }

  /// Implementation of [`Self::dispatch_event`].
  pub(crate) async fn dispatch_event_impl(
    self: &Arc<Self>,
    selector: &str,
    event_type: &str,
    event_init: crate::protocol::SerializedArgument,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .locator(selector)
      .dispatch_event_impl(event_type, event_init, opts)
      .await
  }

  /// Check if an element is editable (not disabled, not readonly).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found.
  #[track_caller]
  pub fn is_editable(
    self: &Arc<Self>,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.main_frame().is_editable(selector)
  }

  // ── Waiting (additional) ────────────────────────────────────────────────

  /// Playwright: `page.waitForFunction(pageFunction, arg?, options?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/page.ts:565`).
  /// Delegates to the main frame, same as Playwright.
  ///
  /// # Errors
  ///
  /// See [`crate::frame::Frame::wait_for_function`].
  #[track_caller]
  pub fn wait_for_function(
    self: &Arc<Self>,
    fn_source: &str,
  ) -> crate::action::Action<'static, crate::options::WaitForFunctionOptions, crate::js_handle::JSHandle> {
    self.main_frame().wait_for_function(fn_source)
  }

  /// [`Self::wait_for_function`] with a serde-serialized argument.
  ///
  /// # Errors
  ///
  /// See [`crate::frame::Frame::wait_for_function_with`].
  #[track_caller]
  pub fn wait_for_function_with(
    self: &Arc<Self>,
    fn_source: &str,
    arg: &(impl serde::Serialize + ?Sized),
  ) -> crate::action::Action<'static, crate::options::WaitForFunctionOptions, crate::js_handle::JSHandle> {
    self.main_frame().wait_for_function_with(fn_source, arg)
  }

  /// [`Self::wait_for_function`] for callers holding an already
  /// serialized argument (the bindings).
  ///
  /// # Errors
  ///
  /// See [`crate::frame::Frame::wait_for_function`].
  pub async fn wait_for_function_serialized(
    self: &Arc<Self>,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
    options: Option<crate::options::WaitForFunctionOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    self
      .main_frame()
      .wait_for_function_serialized(fn_source, arg, is_function, options)
      .await
  }

  /// Playwright: `page.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`. See
  /// [`crate::frame::Frame::wait_for_navigation`], which this is for the
  /// main frame.
  ///
  /// # Errors
  ///
  /// Returns an error on timeout or if the page closes first.
  #[track_caller]
  pub fn wait_for_navigation(
    self: &Arc<Self>,
  ) -> crate::action::Action<'static, crate::options::WaitForNavigationOptions, Option<crate::network::Response>> {
    self.main_frame().wait_for_navigation()
  }

  // ── Mouse (low-level) ──────────────────────────────────────────────────

  /// Scroll via mouse wheel event.
  ///
  /// # Errors
  ///
  /// Returns an error if the wheel event dispatch fails.
  pub(crate) async fn mouse_wheel(&self, delta_x: f64, delta_y: f64) -> Result<()> {
    let (x, y) = *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))?;
    self.inner.mouse_wheel(x, y, delta_x, delta_y).await
  }

  /// Mouse button down (without up). For custom drag sequences.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse down dispatch fails.
  pub(crate) async fn mouse_down(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    self.inner.mouse_down(x, y, button, click_count).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (x, y);
    Ok(())
  }

  /// Mouse button up.
  ///
  /// # Errors
  ///
  /// Returns an error if the mouse up dispatch fails.
  pub(crate) async fn mouse_up(&self, x: f64, y: f64, button: &str, click_count: u32) -> Result<()> {
    self.inner.mouse_up(x, y, button, click_count).await?;
    *self
      .mouse_position
      .lock()
      .map_err(|e| crate::error::FerriError::backend(format!("mouse position lock poisoned: {e}")))? = (x, y);
    Ok(())
  }

  /// Bring this page to front (focus).
  ///
  /// # Errors
  ///
  /// Returns an error if the page cannot be focused.
  pub async fn bring_to_front(&self) -> Result<()> {
    self
      .traced("bringToFront", serde_json::json!({}), async {
        if self.is_closed() {
          return Err(crate::FerriError::target_closed(None));
        }
        self.inner.bring_to_front().await
      })
      .await
  }

  // ── Frames (sync, Playwright parity — task 3.8) ──────────────────────
  //
  // Mirrors Playwright client/page.ts:258 (mainFrame), :273 (frames),
  // :262 (frame). All read from the page-owned `FrameCache` seeded by
  // [`Page::init_frame_cache`] and kept fresh by the listener task.

  /// Main frame of this page. Mirrors Playwright's `page.mainFrame():
  /// Frame` (non-null): the frame cache is built around the main frame
  /// when the backend creates the page, so there always is one.
  #[must_use]
  pub fn main_frame(self: &Arc<Self>) -> Frame {
    Frame::new(Arc::clone(self), self.with_frame_cache(FrameCache::main_frame_id))
  }

  /// All non-detached frames attached to the page, main-frame first.
  /// Sync — reads the cache.
  #[must_use]
  pub fn frames(self: &Arc<Self>) -> Vec<Frame> {
    let ids: Vec<_> = self.with_frame_cache(|c| c.live_ids().collect());
    ids.into_iter().map(|id| Frame::new(Arc::clone(self), id)).collect()
  }

  /// Locate a frame by name or URL. Sync — reads the cache.
  /// Playwright: `page.frame(string | { name?, url? })` — see
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:2755`.
  ///
  /// # Panics
  ///
  /// Panics if `selector` specifies neither `name` nor `url` — matches
  /// Playwright's `assert(name || url, 'Either name or url matcher should be specified')`.
  #[must_use]
  pub fn frame(self: &Arc<Self>, selector: impl Into<FrameSelector>) -> Option<Frame> {
    let sel = selector.into();
    assert!(!sel.is_empty(), "Either name or url matcher should be specified");
    self.with_frame_cache(|c| {
      for id in c.live_ids() {
        let Some(rec) = c.record(&id) else { continue };
        if let Some(name) = &sel.name
          && rec.info.name != *name
        {
          continue;
        }
        if let Some(url) = &sel.url
          && rec.info.url != *url
        {
          continue;
        }
        return Some(Frame::new(Arc::clone(self), id));
      }
      None
    })
  }
}
