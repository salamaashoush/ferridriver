//! Interaction and mutation actions on [`ElementHandle`] (click, fill, check, press, select, upload, screenshot).

use std::sync::Arc;

use crate::error::{FerriError, Result};
use crate::protocol::SerializedArgument;

use super::{ElementHandle, empty_arg};

impl ElementHandle {
  // ── Actions (Phase E) ─────────────────────────────────────────────────

  /// Playwright: `elementHandle.click(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:122`).
  /// Delegates through the temp-tag Locator bridge so every option
  /// (button, clickCount, delay, modifiers, position, force, trial,
  /// timeout) is honored by the shared Locator click dispatch.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `click` error.
  #[track_caller]
  pub fn click(&self) -> crate::action::Action<'static, crate::options::ClickOptions, ()> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.click_impl(Some(opts)).await }))
  }

  pub(crate) async fn click_impl(&self, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    self
      .page()
      .traced_as(
        "ElementHandle",
        "click",
        serde_json::json!({}),
        self.click_untraced(opts),
      )
      .await
  }

  async fn click_untraced(&self, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.click_impl(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.dblclick(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:126`).
  /// Delegates through the temp-tag Locator bridge.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `dblclick` error.
  #[track_caller]
  pub fn dblclick(&self) -> crate::action::Action<'static, crate::options::DblClickOptions, ()> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.dblclick_impl(Some(opts)).await }))
  }

  pub(crate) async fn dblclick_impl(&self, opts: Option<crate::options::DblClickOptions>) -> Result<()> {
    self
      .page()
      .traced_as(
        "ElementHandle",
        "dblclick",
        serde_json::json!({}),
        self.dblclick_untraced(opts),
      )
      .await
  }

  async fn dblclick_untraced(&self, opts: Option<crate::options::DblClickOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.dblclick_impl(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.hover(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:118`).
  /// Delegates through the temp-tag Locator bridge.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `hover` error.
  #[track_caller]
  pub fn hover(&self) -> crate::action::Action<'static, crate::options::HoverOptions, ()> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.hover_impl(Some(opts)).await }))
  }

  pub(crate) async fn hover_impl(&self, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    self
      .page()
      .traced_as(
        "ElementHandle",
        "hover",
        serde_json::json!({}),
        self.hover_untraced(opts),
      )
      .await
  }

  async fn hover_untraced(&self, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.hover_impl(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.type(text, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:159`).
  /// Delegates through the temp-tag Locator bridge so `delay` and
  /// `timeout` are honored by the shared `Locator::type` dispatch.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `type` error.
  #[track_caller]
  pub fn type_str(&self, text: &str) -> crate::action::Action<'static, crate::options::TypeOptions, ()> {
    let handle = self.clone();
    let text = text.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.type_str_impl(&text, Some(opts)).await }))
  }

  pub(crate) async fn type_str_impl(&self, text: &str, opts: Option<crate::options::TypeOptions>) -> Result<()> {
    self
      .page()
      .traced_as(
        "ElementHandle",
        "type",
        serde_json::json!({ "text": text }),
        self.type_str_untraced(text, opts),
      )
      .await
  }

  async fn type_str_untraced(&self, text: &str, opts: Option<crate::options::TypeOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.type_impl(text, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.focus()`. JS `el.focus()`.
  ///
  /// # Errors
  ///
  /// Forwards page-side / protocol error.
  pub async fn focus(&self) -> Result<()> {
    self
      .page()
      .traced_as("ElementHandle", "focus", serde_json::json!({}), async {
        self.ensure_live()?;
        self
          .js_handle
          .evaluate("el => { el.focus(); }", empty_arg(), Some(true))
          .await
          .map(|_| ())
      })
      .await
  }

  /// Playwright: `elementHandle.scrollIntoViewIfNeeded()`. Delegates
  /// through the backend's native `scrollIntoViewIfNeeded` path.
  ///
  /// # Errors
  ///
  /// Forwards the backend's scroll error.
  pub async fn scroll_into_view_if_needed(&self) -> Result<()> {
    self
      .page()
      .traced_as(
        "ElementHandle",
        "scrollIntoViewIfNeeded",
        serde_json::json!({}),
        async {
          self.ensure_live()?;
          self.any_element().scroll_into_view().await
        },
      )
      .await
  }

  /// Playwright: `elementHandle.screenshot(opts?): Promise<Buffer>`.
  /// Captures the element's bounding rect via the backend's native
  /// element screenshot path.
  ///
  /// # Errors
  ///
  /// Forwards the backend's screenshot error.
  #[track_caller]
  pub fn screenshot(&self) -> crate::action::Action<'static, crate::options::ElementScreenshotOptions, Vec<u8>> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.screenshot_impl(opts).await }))
  }

  pub(crate) async fn screenshot_impl(&self, opts: crate::options::ElementScreenshotOptions) -> Result<Vec<u8>> {
    self.ensure_live()?;
    let request = crate::screenshotter::Request::from(&opts);
    let format = request.validate(None)?;
    let page = Arc::clone(self.page());
    page.inner.ensure_engine_injected().await?;
    let timeout_ms = opts.timeout.unwrap_or_else(|| page.default_timeout());
    let deadline = (timeout_ms > 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms));
    let _turn = page.screenshot_turn().await?;
    let mut attempt = 0usize;
    let bytes = crate::operation_budget::scoped(
      timeout_ms,
      || "screenshot".into(),
      Box::pin(async {
        loop {
          let backoff = crate::locator::Locator::RETRY_BACKOFFS_MS
            [attempt.min(crate::locator::Locator::RETRY_BACKOFFS_MS.len() - 1)];
          attempt += 1;
          if backoff > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
          }
          if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            return Err(FerriError::timeout("screenshot", timeout_ms));
          }
          match crate::screenshotter::screenshot_resolved_element(&page, self.any_element(), format, &request).await {
            Err(e) if e.to_string().contains("error:notvisible") => {},
            other => break other,
          }
        }
      }),
    )
    .await?;
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

  // ── Action methods (Playwright parity — delegate via temp-tag) ─────────

  /// Playwright: `elementHandle.fill(value, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:139`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `fill` error.
  #[track_caller]
  pub fn fill(&self, value: &str) -> crate::action::Action<'static, crate::options::FillOptions, ()> {
    let handle = self.clone();
    let value = value.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.fill_impl(&value, Some(opts)).await }))
  }

  pub(crate) async fn fill_impl(&self, value: &str, opts: Option<crate::options::FillOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.fill_impl(value, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.check(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:167`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `check` error.
  #[track_caller]
  pub fn check(&self) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.check_impl(Some(opts)).await }))
  }

  pub(crate) async fn check_impl(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.check_impl(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.uncheck(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:171`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `uncheck` error.
  #[track_caller]
  pub fn uncheck(&self) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.uncheck_impl(Some(opts)).await }))
  }

  pub(crate) async fn uncheck_impl(&self, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.uncheck_impl(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.setChecked(checked, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:175`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `set_checked` error.
  #[track_caller]
  pub fn set_checked(&self, checked: bool) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.set_checked_impl(checked, Some(opts)).await }))
  }

  pub(crate) async fn set_checked_impl(&self, checked: bool, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.set_checked_impl(checked, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.tap(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:130`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `tap` error.
  #[track_caller]
  pub fn tap(&self) -> crate::action::Action<'static, crate::options::TapOptions, ()> {
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.tap_impl(Some(opts)).await }))
  }

  pub(crate) async fn tap_impl(&self, opts: Option<crate::options::TapOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.tap_impl(opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.press(key, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:163`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `press` error.
  #[track_caller]
  pub fn press(&self, key: &str) -> crate::action::Action<'static, crate::options::PressOptions, ()> {
    let handle = self.clone();
    let key = key.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.press_impl(&key, Some(opts)).await }))
  }

  pub(crate) async fn press_impl(&self, key: &str, opts: Option<crate::options::PressOptions>) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.press_impl(key, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.dispatchEvent(type, eventInit)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:110`).
  /// `eventInit` mirrors Playwright's `EventInit` — a plain JS object
  /// merged into the event's `{bubbles, cancelable, composed}` defaults.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `dispatch_event` error.
  #[track_caller]
  pub fn dispatch_event(
    &self,
    event_type: &str,
    event_init: crate::protocol::SerializedArgument,
  ) -> crate::action::Action<'static, crate::options::DispatchEventOptions, ()> {
    let handle = self.clone();
    let event_type = event_type.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { handle.dispatch_event_impl(&event_type, event_init, Some(opts)).await })
    })
  }

  pub(crate) async fn dispatch_event_impl(
    &self,
    event_type: &str,
    event_init: crate::protocol::SerializedArgument,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.dispatch_event_impl(event_type, event_init, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.selectOption(values, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:134`).
  ///
  /// # Errors
  ///
  /// Forwards Locator's `select_option` error.
  #[track_caller]
  pub fn select_option(
    &self,
    values: impl Into<crate::options::SelectOptionValues>,
  ) -> crate::action::Action<'static, crate::options::SelectOptionOptions, Vec<String>> {
    let values = values.into().0;
    let handle = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { handle.select_option_impl(values, Some(opts)).await }))
  }

  pub(crate) async fn select_option_impl(
    &self,
    values: Vec<crate::options::SelectOptionValue>,
    opts: Option<crate::options::SelectOptionOptions>,
  ) -> Result<Vec<String>> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.select_option_impl(values, opts).await;
    self.temp_untag().await;
    result
  }

  /// Playwright: `elementHandle.selectText(options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:143`).
  /// Focuses the element and selects all its text — `<input>` /
  /// `<textarea>` via `select()`, content-editable via a document
  /// range, anything else falls through without error.
  ///
  /// # Errors
  ///
  /// Forwards page-side / protocol errors.
  pub async fn select_text(&self) -> Result<()> {
    self.ensure_live()?;
    self
      .js_handle
      .evaluate(
        "el => { \
          if (!el) return; \
          el.focus(); \
          if (el.isContentEditable) { \
            const sel = document.getSelection(); \
            if (sel) { sel.removeAllRanges(); const r = document.createRange(); r.selectNodeContents(el); sel.addRange(r); } \
          } else if (typeof el.select === 'function') { \
            el.select(); \
          } else if (typeof el.setSelectionRange === 'function') { \
            el.setSelectionRange(0, el.value ? el.value.length : 0); \
          } \
        }",
        SerializedArgument::default(),
        Some(true),
      )
      .await
      .map(|_| ())
  }

  /// Playwright: `elementHandle.setInputFiles(files, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/elementHandle.ts:147`).
  /// Delegates to [`crate::locator::Locator::set_input_files`] through
  /// the temp-tag bridge so path / payload handling is shared with the
  /// locator action path.
  ///
  /// # Errors
  ///
  /// Forwards Locator's `set_input_files` error.
  #[track_caller]
  pub fn set_input_files(
    &self,
    files: impl Into<crate::options::InputFiles>,
  ) -> crate::action::Action<'static, crate::options::SetInputFilesOptions, ()> {
    let files = files.into();
    let handle = self.clone();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { handle.set_input_files_impl(files, Some(opts)).await })
    })
  }

  pub(crate) async fn set_input_files_impl(
    &self,
    files: crate::options::InputFiles,
    opts: Option<crate::options::SetInputFilesOptions>,
  ) -> Result<()> {
    self.ensure_live()?;
    let nonce = self.temp_tag().await?;
    let locator = self.page().locator(&Self::temp_selector(&nonce));
    let result = locator.set_input_files_impl(files, opts).await;
    self.temp_untag().await;
    result
  }
}
