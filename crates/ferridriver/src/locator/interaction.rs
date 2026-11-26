//! Locator interaction methods, waiting, screenshot, drag, and drop.

use std::sync::Arc;

use crate::actions;
use crate::error::Result;
use crate::options::WaitOptions;
use crate::selectors;

use super::Locator;
use super::frame_locator::{DISPATCH_EVENT_BODY, DROP_BODY, SET_INPUT_FILES_BODY, lower_drop_files};

impl Locator {
  // ── Actions ───────────────────────────────────────────────────────────────
  //
  // All action methods use the `retry_resolve!` macro which:
  //   1. Pre-builds selector JS once (no re-parsing per retry)
  //   2. Borrows `&AnyPage` from self — zero AnyPage clones
  //   3. Borrows `&str` parameters directly — zero String clones
  //   4. Expands inline — no closure/future type-erasure overhead

  /// Click the element matched by this locator with the full
  /// [`crate::options::ClickOptions`] surface.
  ///
  /// All options (`button`, `click_count`, `delay`, `force`, `modifiers`,
  /// `position`, `steps`, `trial`, `timeout`) are honored across all
  /// four backends; `no_wait_after` is accepted for signature parity
  /// but has no effect (ferridriver does not implicitly wait for
  /// navigation after click).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, is not actionable
  /// (unless `force=true`), or the click dispatch fails.
  #[track_caller]
  pub fn click(&self) -> crate::action::Action<'static, crate::options::ClickOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.click_impl(Some(opts)).await }))
  }

  pub(crate) async fn click_impl(&self, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    if !self.is_tracing_active() {
      return self.click_fast(&opts, "click").await;
    }
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "click", |el, page| async move {
      let snap = page.nav_snapshot();
      actions::click_with_opts(&el, page, opts_ref).await?;
      if !opts_ref.is_trial() {
        page.settle_navigation(snap, 2_000).await;
      }
      Ok::<(), crate::error::FerriError>(())
    })
  }

  /// Double-click the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the double-click fails.
  #[track_caller]
  pub fn dblclick(&self) -> crate::action::Action<'static, crate::options::DblClickOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.dblclick_impl(Some(opts)).await }))
  }

  pub(crate) async fn dblclick_impl(&self, opts: Option<crate::options::DblClickOptions>) -> Result<()> {
    let click_opts = opts.unwrap_or_default().into_click_options();
    if !self.is_tracing_active() {
      return self.click_fast(&click_opts, "dblclick").await;
    }
    let click_opts_ref = &click_opts;
    retry_resolve!(self, click_opts_ref.timeout, "dblclick", |el, page| async move {
      let snap = page.nav_snapshot();
      actions::click_with_opts(&el, page, click_opts_ref).await?;
      if !click_opts_ref.is_trial() {
        page.settle_navigation(snap, 2_000).await;
      }
      Ok::<(), crate::error::FerriError>(())
    })
  }

  /// Right-click (context menu click) on the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, its bounding box
  /// cannot be computed, or the right-click dispatch fails.
  pub async fn right_click(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "right_click",
      |el, page| async move {
        let center = el.call_js_fn_value(
        "function() { this.scrollIntoViewIfNeeded ? this.scrollIntoViewIfNeeded() : this.scrollIntoView({block: 'center', inline: 'center'}); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
      ).await?;
        if let Some(c) = center {
          let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
          let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
          page.click_at_opts(x, y, "right", 1).await?;
        }
        Ok::<(), crate::error::FerriError>(())
      }
    )
  }

  /// Fill an input or textarea element with the given value.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not a fillable element.
  #[track_caller]
  pub fn fill(&self, value: &str) -> crate::action::Action<'static, crate::options::FillOptions, ()> {
    let locator = self.clone();
    let value = value.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.fill_impl(&value, Some(opts)).await }))
  }

  pub(crate) async fn fill_impl(&self, value: &str, opts: Option<crate::options::FillOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    if let Some(res) = self.fill_fast(value, &opts).await {
      return res;
    }
    let force = opts.is_force();
    let opts_ref = &opts;
    retry_resolve!(
      self,
      opts_ref.timeout,
      "fill",
      ::serde_json::json!({ "value": value }),
      |el, page| async move { actions::fill(&el, page, value, force).await }
    )
  }

  /// Clear the value of an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn clear(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "clear",
      |el, _page| async move {
        el.call_js_fn(
          "function() { \
        if (window.__fd) window.__fd.clearAndDispatch(this); \
        else { this.value = ''; } \
      }",
        )
        .await?;
        Ok::<(), crate::error::FerriError>(())
      }
    )
  }

  /// Show the element-highlight overlay for this locator's selector.
  ///
  /// Playwright:
  /// `highlight(options: { style?: string | Record<string, string | number> }): Promise<Disposable>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:158`).
  /// The optional `style` is collapsed to a CSS declaration string (see
  /// [`crate::options::HighlightStyle::to_css_string`]) and applied to the
  /// highlight box. The overlay re-resolves the selector on each animation
  /// frame, so no element wait happens here — matching Playwright, which
  /// just forwards to `frame._highlight`. Returns a
  /// [`crate::disposable::Disposable`] whose `dispose()` hides the overlay
  /// (Playwright returns a `DisposableStub` wrapping `hideHighlight`).
  ///
  /// # Errors
  ///
  /// Returns an error if selector parsing, frame resolution, or the
  /// injected `addHighlight` call fails.
  pub async fn highlight(
    &self,
    style: Option<crate::options::HighlightStyle>,
  ) -> Result<crate::disposable::Disposable> {
    let (frame, selector) = self.resolved().await?;
    let css = style.as_ref().map(crate::options::HighlightStyle::to_css_string);
    frame.highlight(&selector, css.as_deref()).await?;
    let this = self.clone();
    Ok(crate::disposable::Disposable::new(move || async move {
      match this.hide_highlight().await {
        Ok(()) => Ok(()),
        Err(e) if e.is_target_closed_error() => Ok(()),
        Err(e) => Err(e),
      }
    }))
  }

  /// Hide the element-highlight overlay shown by [`Locator::highlight`].
  ///
  /// Playwright: `hideHighlight(): Promise<void>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:164`).
  /// Removes this locator's highlight; others stay.
  ///
  /// # Errors
  ///
  /// Returns an error if frame resolution or JS evaluation fails.
  pub async fn hide_highlight(&self) -> Result<()> {
    let (frame, selector) = self.resolved().await?;
    frame.hide_highlight(&selector).await
  }

  /// Type text into the element character by character using keyboard events.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or key dispatch fails.
  #[track_caller]
  pub fn r#type(&self, text: &str) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let locator = self.clone();
    let text = text.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.type_impl(&text, Some(opts)).await }))
  }

  pub(crate) async fn type_impl(&self, text: &str, opts: Option<crate::options::TypeOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let delay_ms = opts.resolved_delay_ms();
    let timeout_ms = opts.timeout;
    retry_resolve!(
      self,
      timeout_ms,
      "type",
      ::serde_json::json!({ "text": text }),
      |el, page| async move {
        actions::wait_for_actionable(&el, page).await.ok();
        if delay_ms > 0 {
          // With a per-char delay, fall back to the character-by-character
          // keyboard dispatch (same code path `pressSequentially` uses).
          for ch in text.chars() {
            page.type_char(ch).await?;
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
          }
          Ok(())
        } else {
          el.type_str(text).await
        }
      }
    )
  }

  /// Press a key or key combination (e.g. "Enter", "Control+a").
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the key press fails.
  #[track_caller]
  pub fn press(&self, key: &str) -> crate::action::Action<'static, crate::options::PressOptions, ()> {
    let locator = self.clone();
    let key = key.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.press_impl(&key, Some(opts)).await }))
  }

  pub(crate) async fn press_impl(&self, key: &str, opts: Option<crate::options::PressOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let delay_ms = opts.resolved_delay_ms();
    let timeout_ms = opts.timeout;
    retry_resolve!(
      self,
      timeout_ms,
      "press",
      ::serde_json::json!({ "key": key }),
      |el, page| async move {
        actions::wait_for_actionable(&el, page).await.ok();
        // Focus the element before dispatching keys so the event lands at
        // the intended target (`_press` → `_focus` → `keyboard.press`).
        // Without this the key dispatches to whatever's currently focused,
        // usually the body, and the element under the locator never sees
        // it.
        el.call_js_fn("function() { this.focus(); }").await?;
        page.press_key(key, delay_ms).await
      }
    )
  }

  /// Hover over the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the hover action fails.
  #[track_caller]
  pub fn hover(&self) -> crate::action::Action<'static, crate::options::HoverOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.hover_impl(Some(opts)).await }))
  }

  pub(crate) async fn hover_impl(&self, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "hover", |el, page| async move {
      actions::hover_with_opts(&el, page, opts_ref).await
    })
  }

  /// Focus the element matched by this locator.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn focus(&self) -> Result<()> {
    retry_resolve!(
      self,
      ::std::option::Option::<u64>::None,
      "focus",
      |el, _page| async move {
        el.call_js_fn("function() { this.focus(); }").await?;
        Ok::<(), crate::error::FerriError>(())
      }
    )
  }

  /// Check a checkbox or radio button if it is not already checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  #[track_caller]
  pub fn check(&self) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.check_impl(Some(opts)).await }))
  }

  pub(crate) async fn check_impl(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.set_checked_impl(true, opts).await
  }

  /// Uncheck a checkbox if it is currently checked.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not actionable.
  #[track_caller]
  pub fn uncheck(&self) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.uncheck_impl(Some(opts)).await }))
  }

  pub(crate) async fn uncheck_impl(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.set_checked_impl(false, opts).await
  }

  /// Set the checked state of a checkbox or radio button to match
  /// `checked`. Reads the element's current `checked` property; if it
  /// already matches the target state, the call is a no-op (but
  /// actionability checks still run). Otherwise dispatches a real click
  /// via [`actions::click_with_opts`] so the page sees `input` /
  /// `change` events with the correct timing.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found, is not
  /// actionable, or the click dispatch fails.
  #[track_caller]
  pub fn set_checked(&self, checked: bool) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.set_checked_impl(checked, Some(opts)).await }))
  }

  pub(crate) async fn set_checked_impl(&self, checked: bool, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    if !self.is_tracing_active() {
      return self.set_checked_fast(checked, &opts).await;
    }
    let trial = opts.is_trial();
    // Lower to ClickOptions for the shared click dispatch path so
    // `force` / `trial` / `position` / `timeout` all flow through.
    let click_opts = opts.into_click_options();
    let click_opts_ref = &click_opts;
    retry_resolve!(self, click_opts_ref.timeout, "check", |el, page| async move {
      // setChecked flow:
      //   1. Read current checked state (via `fd.getChecked`, which
      //      understands `input[type=checkbox|radio]` AND ARIA
      //      `aria-checked` roles — `this.checked` alone misses the
      //      latter).
      //   2. If current already matches target → done, no click.
      //   3. Uncheck of a checked radio → hard error (radios only
      //      toggle off by selecting another in their group).
      //   4. Dispatch the click with the caller's options.
      //   5. If `trial` → done (skip verification).
      //   6. Re-read state; if it still doesn't match the target →
      //      `"Clicking the checkbox did not change its state"`.
      let fd = page.injected_script().await?;
      let state_js = format!(
        "function() {{ \
           var r = {fd}.getChecked(this); \
           var isRadio = this.nodeName === 'INPUT' && this.type === 'radio'; \
           return JSON.stringify({{ state: r, isRadio: isRadio }}); \
         }}"
      );
      let read_state = async || -> crate::error::Result<(Option<bool>, bool)> {
        let raw = el
          .call_js_fn_value(&state_js)
          .await?
          .and_then(|v| v.as_str().map(std::string::ToString::to_string))
          .unwrap_or_default();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({}));
        let is_radio = parsed
          .get("isRadio")
          .and_then(serde_json::Value::as_bool)
          .unwrap_or(false);
        let state_val = match parsed.get("state") {
          Some(v) if v.is_boolean() => Some(v.as_bool().unwrap_or(false)),
          _ => None,
        };
        Ok((state_val, is_radio))
      };

      let (current, is_radio) = read_state().await?;
      let Some(current) = current else {
        return Err(crate::error::FerriError::invalid_argument(
          "element",
          "not a checkbox, radio button, or ARIA-checkable element",
        ));
      };
      if current == checked {
        return Ok::<(), crate::error::FerriError>(());
      }
      if !checked && is_radio {
        return Err(crate::error::FerriError::invalid_argument(
          "element",
          "Cannot uncheck radio button. Radio buttons can only be unchecked by selecting another radio button in the same group.",
        ));
      }
      actions::click_with_opts(&el, page, click_opts_ref).await?;
      if trial {
        return Ok::<(), crate::error::FerriError>(());
      }
      let (new_state, _) = read_state().await?;
      if new_state != Some(checked) {
        return Err(crate::error::FerriError::backend(
          "clicking the checkbox did not change its state",
        ));
      }
      Ok::<(), crate::error::FerriError>(())
    })
  }

  /// Tap the element (touch event). Dispatches touchstart + touchend on platforms
  /// that support Touch/TouchEvent APIs, falls back to pointerdown + pointerup + click
  /// on desktop browsers (e.g. Playwright `WebKit`) where Touch constructors are unavailable.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the tap event dispatch fails.
  #[track_caller]
  pub fn tap(&self) -> crate::action::Action<'static, crate::options::TapOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.tap_impl(Some(opts)).await }))
  }

  pub(crate) async fn tap_impl(&self, opts: Option<crate::options::TapOptions>) -> Result<()> {
    let opts = opts.unwrap_or_default();
    let opts_ref = &opts;
    retry_resolve!(self, opts_ref.timeout, "tap", |el, page| async move {
      actions::tap_with_opts(&el, page, opts_ref).await
    })
  }

  /// Select all text in an input or textarea element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or the selection fails.
  pub async fn select_text(&self) -> Result<()> {
    let el = self.resolve().await?;
    el.call_js_fn(
      "function() { \
      this.focus(); \
      if (this.select) { this.select(); } \
      else if (this.setSelectionRange) { this.setSelectionRange(0, this.value ? this.value.length : 0); } \
    }",
    )
    .await
  }

  /// Select an `<option>` by value within a `<select>` element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or is not a `<select>`.
  #[track_caller]
  pub fn select_option(
    &self,
    values: impl Into<crate::options::SelectOptionValues>,
  ) -> crate::action::Action<'static, crate::options::SelectOptionOptions, Vec<String>> {
    let values = values.into().0;
    let locator = self.clone();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.select_option_impl(values, Some(opts)).await })
    })
  }

  pub(crate) async fn select_option_impl(
    &self,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    let opts = opts.unwrap_or_default();
    let timeout_ms = opts.timeout;
    let force = opts.force.unwrap_or(false);
    let values_ref = &values;
    // Mirrors Playwright's `server/dom.ts::_selectOption`: when not
    // `force`, gate the dispatch on `checkElementStates(['visible',
    // 'enabled'])` so a hidden or disabled `<select>` returns the
    // `error:not<state>` retriable marker until the deadline fires.
    // `force: true` skips the pre-check and goes straight to the
    // injected `selectOptions` call.
    retry_resolve!(
      self,
      timeout_ms,
      "selectOption",
      ::serde_json::json!({ "values": values_ref }),
      |el, page| async move {
        if !force {
          let fd = page.injected_script().await?;
          let state_raw = el
            .call_js_fn_value(&format!(
              "function() {{ return {fd}.checkElementStates(this, ['visible', 'enabled']); }}"
            ))
            .await?
            .and_then(|v| v.as_str().map(std::string::ToString::to_string))
            .unwrap_or_else(|| "error:notconnected".to_string());
          if state_raw != "done" {
            return Err(crate::error::FerriError::backend(state_raw));
          }
        }
        actions::select_options(&el, page, values_ref).await
      }
    )
  }

  /// Set file paths on a file input element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not a file input or the upload fails.
  #[track_caller]
  pub fn set_input_files(
    &self,
    files: impl Into<crate::options::InputFiles>,
  ) -> crate::action::Action<'static, crate::options::SetInputFilesOptions, ()> {
    let files = files.into();
    let locator = self.clone();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.set_input_files_impl(files, Some(opts)).await })
    })
  }

  pub(crate) async fn set_input_files_impl(
    &self,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    let timeout_ms = opts
      .and_then(|o| o.timeout)
      .unwrap_or_else(|| self.frame.page_arc().default_timeout());
    // Playwright's `dom.ts::_setInputFiles`: payloads become `File`
    // objects in the page and never touch a disk, and only paths go to
    // the browser as paths. An empty list is no paths, so it clears the
    // input in the page as well (`client/elementHandle.ts::convertInputFiles`).
    // A browser on a device cannot open a path of this machine, so there
    // the files are read here and sent as payloads too.
    let paths = match files {
      crate::options::InputFiles::Paths(paths) if !paths.is_empty() && self.shares_host_files().await => paths,
      files => {
        let payloads = serde_json::Value::Array(lower_drop_files(Some(files))?);
        let names: Vec<&str> = payloads
          .as_array()
          .into_iter()
          .flatten()
          .filter_map(|file| file.get("name").and_then(serde_json::Value::as_str))
          .collect();
        let js = format!("function() {{ const payloads = {payloads};{SET_INPUT_FILES_BODY} }}");
        let js_ref = js.as_str();
        return retry_resolve!(
          self,
          Some(timeout_ms),
          "setInputFiles",
          ::serde_json::json!({ "files": names }),
          |el, _page| async move { el.call_js_fn(js_ref).await }
        );
      },
    };
    let paths: Vec<String> = paths.into_iter().map(|p| p.display().to_string()).collect();

    // Resolve the element through the standard action retry funnel and
    // address it directly (mirrors Playwright's
    // `setInputFilePaths(handle, paths)`). The funnel resolves in the
    // locator's owning frame with full selector-engine support, and a
    // stale node between resolve and the protocol call (hidden,
    // framework-managed inputs that re-mount mid-flow) surfaces as a
    // retryable error that re-resolves fresh instead of escaping as a
    // raw "Object id doesn't reference a Node".
    let paths_ref = &paths;
    retry_resolve!(
      self,
      Some(timeout_ms),
      "setInputFiles",
      ::serde_json::json!({ "files": paths_ref }),
      |el, page| async move { page.set_input_files_element(&el, paths_ref).await }
    )
  }

  /// Whether a path on this machine names the same file to the browser
  /// behind this locator. A page that belongs to no context is a local
  /// one.
  async fn shares_host_files(&self) -> bool {
    match self.frame.page_arc().context() {
      Some(context) => context.shares_host_files().await,
      None => true,
    }
  }

  /// Scroll the element into the visible area of the viewport.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or scroll fails.
  pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
    retry_resolve!(self, None, "scrollIntoViewIfNeeded", |el, _page| async move {
      el.scroll_into_view().await
    })
  }

  /// Dispatch a DOM event of the given type on the element. Mirrors
  /// Playwright's `frames.ts::dispatchEvent` (see
  /// `/tmp/playwright/packages/playwright-core/src/server/frames.ts:847`):
  /// resolve the element under the retry loop (Playwright does NOT run
  /// actionability for dispatchEvent — it's a programmatic dispatch),
  /// then invoke `injectedScript.dispatchEvent` with the matching
  /// constructor. `opts.timeout` flows through to the retry deadline.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Timeout` if the element does not appear
  /// before the deadline.
  #[track_caller]
  pub fn dispatch_event(
    &self,
    event_type: &str,
    event_init: crate::protocol::SerializedArgument,
  ) -> crate::action::Action<'static, crate::options::DispatchEventOptions, ()> {
    let locator = self.clone();
    let event_type = event_type.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.dispatch_event_impl(&event_type, event_init, Some(opts)).await })
    })
  }

  pub(crate) async fn dispatch_event_impl(
    &self,
    event_type: &str,
    event_init: crate::protocol::SerializedArgument,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    let timeout_ms = opts.and_then(|o| o.timeout);
    // `eventInit` is an evaluation argument, as Playwright serializes it
    // (`client/frame.ts` `serializeArgument(eventInit)`), so a JSHandle in
    // it (a `DataTransfer` for a drop) reaches the page as the object it
    // names.
    let event_type_js = serde_json::to_string(event_type)?;
    let source = format!("(el, eventInit) => {{ {DISPATCH_EVENT_BODY} }}").replace("__TYPE__", &event_type_js);
    let source = source.as_str();
    retry_resolve!(
      self,
      timeout_ms,
      "dispatchEvent",
      ::serde_json::json!({ "type": event_type }),
      |el, _page| async {
        let page_arc = Arc::clone(self.frame.page_arc());
        let handle = crate::element_handle::ElementHandle::from_any_element(page_arc, el).await?;
        let result = handle
          .as_js_handle()
          .evaluate(source, event_init.clone(), Some(true))
          .await;
        let _ = handle.dispose().await;
        result.map(|_| ())
      }
    )
  }

  // ── Waiting ───────────────────────────────────────────────────────────────

  /// Wait for the element to reach the specified state.
  ///
  /// Playwright states (`packages/playwright-core/src/client/locator.ts`):
  ///
  /// * `"attached"` — element is present in the DOM. Computed style is
  ///   not consulted. Matches `element.isConnected`.
  /// * `"visible"` — element is attached **and** has non-empty bounding
  ///   box, is not `display:none` / `visibility:hidden` / `opacity:0`.
  /// * `"hidden"` — element is either detached or not visible. A
  ///   detached element satisfies `"hidden"` (Playwright parity).
  /// * `"detached"` — element is not present in the DOM.
  ///
  /// Previously `"attached"` and `"visible"` were conflated — both
  /// returned as soon as a DOM query succeeded. That broke Playwright
  /// tests that rely on `attached` resolving for zero-size or
  /// currently-invisible elements.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout expires before the element reaches
  /// the desired state, or if an unknown state is specified.
  #[track_caller]
  pub fn wait_for(&self) -> crate::action::Action<'static, crate::options::WaitOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.wait_for_impl(opts).await }))
  }

  pub(crate) async fn wait_for_impl(&self, opts: WaitOptions) -> Result<()> {
    // The page's default, not a constant: it is what carries
    // `page.setDefaultTimeout` and, through it, the context's. Every
    // other wait in this file resolves it the same way, and this one
    // did not, so a context default reached everything except the two
    // calls named after waiting.
    let timeout = opts.timeout.unwrap_or_else(|| self.frame.page_arc().default_timeout());
    let state = opts.state.unwrap_or_default();
    // Playwright's `timeout: 0` waits for as long as it takes.
    let deadline = (timeout != 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout));
    let timed_out = || format!("waiting for '{}' to be {state}", self.selector);

    crate::operation_budget::scoped(timeout, timed_out, async move {
      loop {
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
          return Err(crate::error::FerriError::timeout(timed_out(), timeout));
        }
        // Resolve frameLocator enter-frame hops each slice so a child frame
        // that is not yet attached (or was re-attached) is picked up on a
        // later attempt (no-op for plain selectors). A resolution failure
        // means the parent `<iframe>` / child frame is not present: for
        // `attached`/`visible` that is a retry, for `detached`/`hidden`
        // the element is by definition gone.
        let slice_ms = deadline.map_or(Self::WAIT_SLICE_MS, |deadline| {
          u64::try_from(
            deadline
              .saturating_duration_since(tokio::time::Instant::now())
              .as_millis(),
          )
          .unwrap_or(Self::WAIT_SLICE_MS)
          .clamp(1, Self::WAIT_SLICE_MS)
        });
        let answer = match self.resolved().await {
          Ok((rframe, rsel)) => {
            let inner = rframe.page_arc().inner();
            let parts_json = selectors::build_parts_json(&selectors::parse(&rsel)?);
            let js = format!(
              "window.__fd.waitForState({parts_json},{},'{}',{slice_ms})",
              self.strict,
              state.as_str(),
            );
            // A document being replaced has no engine yet: not attached.
            match inner.ensure_engine_injected().await {
              Ok(()) if rframe.is_main_frame() => inner.evaluate(&js).await.ok().flatten(),
              Ok(()) => inner.evaluate_in_frame(&js, rframe.id()).await.ok().flatten(),
              Err(_) => None,
            }
          },
          Err(_) => None,
        };
        let answer = answer
          .as_ref()
          .and_then(serde_json::Value::as_str)
          .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
        match answer
          .as_ref()
          .and_then(|answer| answer.get("done"))
          .and_then(serde_json::Value::as_bool)
        {
          Some(true) => return Ok(()),
          // The page watched for the whole slice and the state never came.
          Some(false) => continue,
          None => {},
        }
        // Playwright's strict `waitForSelector` throws on a second match;
        // waiting on would only end in a timeout that hides it.
        if let Some(count) = answer
          .as_ref()
          .and_then(|answer| answer.get("strict"))
          .and_then(serde_json::Value::as_u64)
        {
          return Err(crate::error::FerriError::strict(
            self.selector.clone(),
            usize::try_from(count).unwrap_or(usize::MAX),
          ));
        }
        // No frame, no engine or no document to ask: nothing is attached.
        if matches!(
          state,
          crate::options::WaitState::Detached | crate::options::WaitState::Hidden
        ) {
          return Ok(());
        }
        crate::abort::sleep(std::time::Duration::from_millis(10)).await;
      }
    })
    .await
  }

  // ── Screenshot ────────────────────────────────────────────────────────────

  /// Take a screenshot of the element (PNG by default; `.format(...)`,
  /// `.path(...)`, `.timeout(...)` chain as setters).
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or screenshot capture fails.
  #[track_caller]
  pub fn screenshot(&self) -> crate::action::Action<'static, crate::options::ElementScreenshotOptions, Vec<u8>> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { Box::pin(locator.screenshot_impl(opts)).await }))
  }

  pub(crate) async fn screenshot_impl(&self, opts: crate::options::ElementScreenshotOptions) -> Result<Vec<u8>> {
    let request = crate::screenshotter::Request::from(&opts);
    let format = request.validate(None)?;
    let page = Arc::clone(self.frame.page_arc());
    let _turn = page.screenshot_turn().await?;
    let (page_ref, request_ref) = (&page, &request);
    let bytes = retry_resolve!(self, opts.timeout, "screenshot", |el, _page| async move {
      crate::screenshotter::screenshot_resolved_element(page_ref, &el, format, request_ref).await
    })?;
    if let Some(ref path) = opts.path {
      if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
      {
        let _ = tokio::fs::create_dir_all(parent).await;
      }
      tokio::fs::write(path, &bytes)
        .await
        .map_err(|e| crate::error::FerriError::Backend(format!("screenshot write {}: {e}", path.display())))?;
    }
    Ok(bytes)
  }

  // ── Editable check ───────────────────────────────────────────────────────

  /// Playwright: `locator.isEditable(options?: TimeoutOptions): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`], and an error when the element is not an
  /// `<input>`, `<textarea>`, `<select>` or editable element.
  #[track_caller]
  pub fn is_editable(&self) -> crate::action::Action<'static, crate::options::TimeoutOptions, bool> {
    let locator = self.clone();
    crate::action::Action::new(move |opts: crate::options::TimeoutOptions| {
      Box::pin(async move { locator.is_editable_impl(opts.timeout).await })
    })
  }

  pub(crate) async fn is_editable_impl(&self, timeout: Option<u64>) -> Result<bool> {
    self.read_state("isEditable", "editable", timeout).await
  }

  // ── Blur ────────────────────────────────────────────────────────────────

  /// Remove focus from the element.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found.
  pub async fn blur(&self) -> Result<()> {
    let el = self.resolve().await?;
    let _ = el.call_js_fn("function() { this.blur(); }").await;
    Ok(())
  }

  // ── Press sequentially ──────────────────────────────────────────────────

  /// Type text character by character with a delay between each.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be found or any key press fails.
  #[track_caller]
  pub fn press_sequentially(&self, text: &str) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let locator = self.clone();
    let text = text.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { locator.press_sequentially_impl(&text, Some(opts)).await })
    })
  }

  pub(crate) async fn press_sequentially_impl(
    &self,
    text: &str,
    opts: Option<crate::options::TypeOptions>,
  ) -> Result<()> {
    // Playwright's `pressSequentially` shares the `TypeOptions` shape
    // with deprecated `type` (same three fields), so route both here.
    self.type_impl(text, opts).await
  }

  // ── Drag to another locator ─────────────────────────────────────────────

  /// Drag this element to `target`. Mirrors Playwright's
  /// `Locator.dragTo(target, options)` signature per
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:13293`.
  ///
  /// When `DragAndDropOptions::source_position` is set, the press point is
  /// the source element's padding-box origin offset by that point; otherwise
  /// the source element's center is used. Same for `target_position` on the
  /// release point. `DragAndDropOptions::steps` controls how many
  /// interpolated `mousemove` events are emitted between press and release
  /// (default `5`; Playwright defaults to `1` but compensates with native
  /// drag interception, which ferridriver does not implement).
  /// `DragAndDropOptions::trial` skips the
  /// actual mouse action, returning after both elements resolve.
  /// `DragAndDropOptions::strict` is ignored here (per Playwright) because
  /// this locator already carries its own strict flag.
  ///
  /// # Errors
  ///
  /// Returns an error if either element cannot be found, bounding box
  /// coordinates cannot be read, or the drag operation fails.
  #[track_caller]
  pub fn drag_to(&self, target: &Locator) -> crate::action::Action<'static, crate::options::DragAndDropOptions, ()> {
    let locator = self.clone();
    let target = target.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.drag_to_impl(&target, Some(opts)).await }))
  }

  pub(crate) async fn drag_to_impl(
    &self,
    target: &Locator,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();
    let steps = opts.steps.unwrap_or(1);
    let opts_ref = &opts;
    // Press on the source, then release on the target, each its own
    // pointer action with its own retries as in Playwright's
    // `dragAndDrop`: the target is found, revealed and checked only once
    // the drag has started, when the page may have moved it.
    let from = retry_resolve!(
      self,
      opts_ref.timeout,
      "dragTo",
      ::serde_json::json!({ "target": target.selector }),
      |el, page| async move { actions::drag_step(&el, page, opts_ref, actions::DragStep::Press).await }
    )?;
    retry_resolve!(target, opts_ref.timeout, "dragTo", |el, page| async move {
      actions::drag_step(&el, page, opts_ref, actions::DragStep::Release { from, steps }).await
    })?;
    Ok(())
  }

  // ── Drop a payload onto this element ────────────────────────────────────

  /// Drop a file/data payload onto this element. Mirrors Playwright's
  /// `Locator.drop(payload, options)` (`client/locator.ts:129`), which
  /// forwards to `frame._drop` -> server `dom.ts::_drop`.
  ///
  /// The drop is performed by constructing a `DataTransfer` carrying the
  /// payload's `File` objects (built from each `FilePayload`'s bytes) and
  /// `data` entries (`DataTransfer.setData(mimeType, value)`), then
  /// dispatching the `dragenter` / `dragover` / `drop` `DragEvent`
  /// sequence on the resolved element at the drop point. Matching
  /// Playwright, if the `dragover` handler does not call `preventDefault()`
  /// the target is treated as rejecting the drop: a `dragleave` is
  /// dispatched and a `FerriError::Backend` is returned.
  ///
  /// `DropOptions::position` offsets the drop point from the element's
  /// padding-box top-left; when absent the element center is used.
  /// `DropOptions::modifiers` are reflected on the dispatched `DragEvent`s'
  /// modifier flags. `DropOptions::timeout` is accepted for signature
  /// parity; the underlying single-shot resolve already honours the
  /// context's default action timeout.
  ///
  /// File paths in `DropPayload::files` are read into memory here (matching
  /// the co-located server path in Playwright) so the page can construct
  /// real `File` objects without filesystem access of its own.
  ///
  /// # Errors
  ///
  /// Returns an error if the element cannot be resolved, a referenced file
  /// path cannot be read, or the target rejects the drop.
  #[track_caller]
  pub fn drop(
    &self,
    payload: crate::options::DropPayload,
  ) -> crate::action::Action<'static, crate::options::DropOptions, ()> {
    let locator = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { locator.drop_impl(payload, Some(opts)).await }))
  }

  pub(crate) async fn drop_impl(
    &self,
    payload: crate::options::DropPayload,
    options: Option<crate::options::DropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();

    // Lower the payload's files into `{name, mimeType, buffer(base64)}`
    // records the page can rebuild into `File` objects. `Paths` are read
    // from disk into buffers (Playwright's co-located server reads
    // localPaths the same way); `Payloads` carry their bytes already.
    let file_records = lower_drop_files(payload.files)?;

    let data_records: Vec<serde_json::Value> = payload
      .data
      .into_iter()
      .map(|(mime_type, value)| serde_json::json!({ "mimeType": mime_type, "value": value }))
      .collect();

    let modifiers = serde_json::json!({
      "alt": opts.modifiers.contains(&crate::options::Modifier::Alt),
      "ctrl": opts.modifiers.iter().any(|m| {
        matches!(m, crate::options::Modifier::Control)
          || (matches!(m, crate::options::Modifier::ControlOrMeta) && !cfg!(target_os = "macos"))
      }),
      "meta": opts.modifiers.iter().any(|m| {
        matches!(m, crate::options::Modifier::Meta)
          || (matches!(m, crate::options::Modifier::ControlOrMeta) && cfg!(target_os = "macos"))
      }),
      "shift": opts.modifiers.contains(&crate::options::Modifier::Shift),
    });

    let position = match opts.position {
      Some(p) => serde_json::json!({ "x": p.x, "y": p.y }),
      None => serde_json::Value::Null,
    };

    let arg = serde_json::json!({
      "payloads": file_records,
      "data": data_records,
      "modifiers": modifiers,
      "position": position,
    });
    let arg_json = serde_json::to_string(&arg)
      .map_err(|e| crate::error::FerriError::Backend(format!("failed to serialise drop payload: {e}")))?;

    let el = self.resolve().await?;
    let function = format!("function() {{ const arg = {arg_json}; {DROP_BODY} }}");
    let result = el.call_js_fn_value(&function).await?;

    match result.as_ref().and_then(serde_json::Value::as_str) {
      Some("accepted") => Ok(()),
      Some("not-accepted") => Err(crate::error::FerriError::Backend(
        "Drop target did not accept the drop -- its dragover handler did not call preventDefault()".into(),
      )),
      Some("error:notconnected") => Err(crate::error::FerriError::Backend(
        "Drop target element is not connected to the document".into(),
      )),
      _ => Err(crate::error::FerriError::Backend(
        "drop did not return a recognised status".into(),
      )),
    }
  }
}
