//! Frame-scoped selector actions and DOM state queries.

use crate::error::Result;
use crate::locator::Locator;

use super::Frame;

impl Frame {
  /// Click the element matched by `selector`. Accepts Playwright's full
  /// `FrameClickOptions` bag (see [`crate::options::ClickOptions`]).
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the click fails.
  #[track_caller]
  pub fn click(&self, selector: &str) -> crate::action::Action<'static, crate::options::ClickOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.click_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn click_impl(&self, selector: &str, opts: Option<crate::options::ClickOptions>) -> Result<()> {
    self.selector_locator(selector).click_impl(opts).await
  }

  /// Double-click the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the dblclick fails.
  #[track_caller]
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
    self.selector_locator(selector).dblclick_impl(opts).await
  }

  /// Hover the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the hover fails.
  #[track_caller]
  pub fn hover(&self, selector: &str) -> crate::action::Action<'static, crate::options::HoverOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.hover_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn hover_impl(&self, selector: &str, opts: Option<crate::options::HoverOptions>) -> Result<()> {
    self.selector_locator(selector).hover_impl(opts).await
  }

  /// Tap (touch) the element matched by `selector`. Mirrors
  /// `frame.tap(selector, options?)` per
  /// `/tmp/playwright/packages/playwright-core/src/client/frame.ts:308`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the tap fails.
  #[track_caller]
  pub fn tap(&self, selector: &str) -> crate::action::Action<'static, crate::options::TapOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.tap_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn tap_impl(&self, selector: &str, opts: Option<crate::options::TapOptions>) -> Result<()> {
    self.selector_locator(selector).tap_impl(opts).await
  }

  /// Focus the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or focus fails.
  pub async fn focus(&self, selector: &str) -> Result<()> {
    self.selector_locator(selector).focus().await
  }

  // -- Form input --------------------------------------------------------

  /// Fill an input matching `selector` with `value`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not fillable.
  #[track_caller]
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
    self.selector_locator(selector).fill_impl(value, opts).await
  }

  /// Type characters into an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or typing fails.
  #[track_caller]
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
    self.selector_locator(selector).type_impl(text, opts).await
  }

  /// Press a key on an element matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the key press fails.
  #[track_caller]
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
    self.selector_locator(selector).press_impl(key, opts).await
  }

  /// Check a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  #[track_caller]
  pub fn check(&self, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.check_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn check_impl(&self, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.selector_locator(selector).check_impl(opts).await
  }

  /// Uncheck a checkbox matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not uncheckable.
  #[track_caller]
  pub fn uncheck(&self, selector: &str) -> crate::action::Action<'static, crate::options::CheckOptions, ()> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.uncheck_impl(&selector, Some(opts)).await }))
  }

  pub(crate) async fn uncheck_impl(&self, selector: &str, opts: Option<crate::options::CheckOptions>) -> Result<()> {
    self.selector_locator(selector).uncheck_impl(opts).await
  }

  /// Set the checked state of a checkbox/radio matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or is not checkable.
  #[track_caller]
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
    self.selector_locator(selector).set_checked_impl(checked, opts).await
  }

  /// Select a `<select>` option in the element matched by `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or the option cannot
  /// be selected.
  #[track_caller]
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
    self.selector_locator(selector).select_option_impl(values, opts).await
  }

  /// Set input files on a `<input type=file>` matching `selector`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element is not found or file setting fails.
  #[track_caller]
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
    self.selector_locator(selector).set_input_files_impl(files, opts).await
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
  #[track_caller]
  pub fn drag_and_drop(
    &self,
    source: &str,
    target: &str,
  ) -> crate::action::Action<'static, crate::options::DragAndDropOptions, ()> {
    let frame = self.clone();
    let source = source.to_string();
    let target = target.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move { Box::pin(frame.drag_and_drop_impl(&source, &target, Some(opts))).await })
    })
  }

  pub(crate) async fn drag_and_drop_impl(
    &self,
    source: &str,
    target: &str,
    options: Option<crate::options::DragAndDropOptions>,
  ) -> Result<()> {
    let opts = options.unwrap_or_default();
    let src = self.selector_locator(source);
    let tgt = self.selector_locator(target);
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
  #[track_caller]
  pub fn dispatch_event(
    &self,
    selector: &str,
    event_type: &str,
    event_init: crate::protocol::SerializedArgument,
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
    event_init: crate::protocol::SerializedArgument,
    opts: Option<crate::options::DispatchEventOptions>,
  ) -> Result<()> {
    self
      .locator(selector)
      .dispatch_event_impl(event_type, event_init, opts)
      .await
  }

  // -- Content / attribute reads ----------------------------------------

  /// The locator a selector read runs through: strict when the call or
  /// the context's `strictSelectors` says so.
  fn read_locator(&self, selector: &str, strict: Option<bool>) -> Locator {
    self
      .locator(selector)
      .strict(strict.unwrap_or_else(|| self.page.strict_selectors()))
  }

  #[track_caller]
  fn selector_read<T: Send + 'static>(
    &self,
    selector: &str,
    read: impl FnOnce(Locator, Option<u64>) -> crate::action::ActionFuture<'static, T> + Send + 'static,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, T> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts: crate::options::SelectorReadOptions| {
      read(frame.read_locator(&selector, opts.strict), opts.timeout)
    })
  }

  /// Playwright: `frame.textContent(selector, options?: { strict?, timeout? }):
  /// Promise<string | null>`. Waits for the element.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Timeout`] if nothing matches in time.
  #[track_caller]
  pub fn text_content(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, Option<String>> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.text_content_impl(timeout).await })
    })
  }

  /// Playwright: `frame.innerText(selector, options?): Promise<string>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn inner_text(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, String> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.inner_text_impl(timeout).await })
    })
  }

  /// Playwright: `frame.innerHTML(selector, options?): Promise<string>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn inner_html(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, String> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.inner_html_impl(timeout).await })
    })
  }

  /// Playwright: `frame.getAttribute(selector, name, options?):
  /// Promise<string | null>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn get_attribute(
    &self,
    selector: &str,
    name: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, Option<String>> {
    let name = name.to_string();
    self.selector_read(selector, move |locator, timeout| {
      Box::pin(async move { locator.get_attribute_impl(&name, timeout).await })
    })
  }

  /// Playwright: `frame.inputValue(selector, options?): Promise<string>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn input_value(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, String> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.input_value_impl(timeout).await })
    })
  }

  // -- State checks ------------------------------------------------------

  /// Playwright: `frame.isVisible(selector, options?): Promise<boolean>`.
  /// Does not wait: `timeout` is accepted and ignored, as Playwright does.
  ///
  /// # Errors
  ///
  /// A strict-mode violation, or a failed evaluation.
  #[track_caller]
  pub fn is_visible(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.selector_read(selector, |locator, _| {
      Box::pin(async move { locator.is_visible_impl().await })
    })
  }

  /// Playwright: `frame.isHidden(selector, options?): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::is_visible`].
  #[track_caller]
  pub fn is_hidden(&self, selector: &str) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.selector_read(selector, |locator, _| {
      Box::pin(async move { locator.is_visible_impl().await.map(|v| !v) })
    })
  }

  /// Playwright: `frame.isEnabled(selector, options?): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn is_enabled(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.is_enabled_impl(timeout).await })
    })
  }

  /// Playwright: `frame.isDisabled(selector, options?): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn is_disabled(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.is_disabled_impl(timeout).await })
    })
  }

  /// Playwright: `frame.isEditable(selector, options?): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn is_editable(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.is_editable_impl(timeout).await })
    })
  }

  /// Playwright: `frame.isChecked(selector, options?): Promise<boolean>`.
  ///
  /// # Errors
  ///
  /// As [`Self::text_content`].
  #[track_caller]
  pub fn is_checked(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, crate::options::SelectorReadOptions, bool> {
    self.selector_read(selector, |locator, timeout| {
      Box::pin(async move { locator.is_checked_impl(timeout).await })
    })
  }
}
