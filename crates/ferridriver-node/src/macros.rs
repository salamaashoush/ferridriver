//! Shared declarative macros that generate identical NAPI surfaces once
//! across `Page`, `Frame`, `Locator`, `FrameLocator`, `ElementHandle`, and
//! the Node `EventEmitter` classes (`Page`, `BrowserContext`, `Browser`,
//! `WebSocket`).
//!
//! Keeping these forwarding surfaces in one place removes ~1,800 lines of
//! 2x–4x duplicated methods and prevents classes from drifting on options or
//! TypeScript signatures.

/// Generate Playwright's `getByRole`, `getByText`, `getByLabel`,
/// `getByPlaceholder`, `getByAltText`, `getByTitle`, `getByTestId`, and
/// `frameLocator` methods on any NAPI wrapper whose `self.inner` exposes
/// the matching core builders.
///
/// Pass `optional_frame_locator` for `Page` / `Frame` (where `selector?` is
/// optional) or `required_frame_locator` for `Locator` / `FrameLocator`.
macro_rules! napi_locator_queries {
  ($target:ident, optional_frame_locator) => {
    $crate::macros::napi_locator_queries!(@common $target);

    #[napi_derive::napi]
    impl $target {
      /// Playwright: `frameLocator(selector?: string): FrameLocator`.
      #[napi(ts_args_type = "selector?: string")]
      pub fn frame_locator(&self, selector: Option<String>) -> $crate::frame_locator::FrameLocator {
        $crate::frame_locator::FrameLocator::wrap(self.inner.frame_locator(selector.as_deref()))
      }
    }
  };
  ($target:ident, required_frame_locator) => {
    $crate::macros::napi_locator_queries!(@common $target);

    #[napi_derive::napi]
    impl $target {
      /// Playwright: `frameLocator(selector: string): FrameLocator`.
      #[napi]
      pub fn frame_locator(&self, selector: String) -> $crate::frame_locator::FrameLocator {
        $crate::frame_locator::FrameLocator::wrap(self.inner.frame_locator(selector.as_str()))
      }
    }
  };
  (@common $target:ident) => {
    #[napi_derive::napi]
    impl $target {
      /// Playwright: `getByRole(role, options?): Locator`.
      #[napi]
      pub fn get_by_role(
        &self,
        role: String,
        options: Option<$crate::types::RoleOptions>,
      ) -> $crate::locator::Locator {
        let opts = options.map(ferridriver::options::RoleOptions::from);
        $crate::locator::Locator::wrap(
          self
            .inner
            .get_by_role(role.as_str())
            .maybe_options(opts)
            .into_locator(),
        )
      }

      /// Playwright: `getByText(text: string | RegExp, options?): Locator`.
      #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
      pub fn get_by_text(
        &self,
        text: napi::Either<String, $crate::types::JsRegExpLike>,
        options: Option<$crate::types::TextOptions>,
      ) -> $crate::locator::Locator {
        let opts = options.map(ferridriver::options::TextOptions::from);
        $crate::locator::Locator::wrap(
          self
            .inner
            .get_by_text($crate::types::getby_input_to_rust(text))
            .maybe_options(opts)
            .into_locator(),
        )
      }

      /// Playwright: `getByLabel(text: string | RegExp, options?): Locator`.
      #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
      pub fn get_by_label(
        &self,
        text: napi::Either<String, $crate::types::JsRegExpLike>,
        options: Option<$crate::types::TextOptions>,
      ) -> $crate::locator::Locator {
        let opts = options.map(ferridriver::options::TextOptions::from);
        $crate::locator::Locator::wrap(
          self
            .inner
            .get_by_label($crate::types::getby_input_to_rust(text))
            .maybe_options(opts)
            .into_locator(),
        )
      }

      /// Playwright: `getByPlaceholder(text: string | RegExp, options?): Locator`.
      #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
      pub fn get_by_placeholder(
        &self,
        text: napi::Either<String, $crate::types::JsRegExpLike>,
        options: Option<$crate::types::TextOptions>,
      ) -> $crate::locator::Locator {
        let opts = options.map(ferridriver::options::TextOptions::from);
        $crate::locator::Locator::wrap(
          self
            .inner
            .get_by_placeholder($crate::types::getby_input_to_rust(text))
            .maybe_options(opts)
            .into_locator(),
        )
      }

      /// Playwright: `getByAltText(text: string | RegExp, options?): Locator`.
      #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
      pub fn get_by_alt_text(
        &self,
        text: napi::Either<String, $crate::types::JsRegExpLike>,
        options: Option<$crate::types::TextOptions>,
      ) -> $crate::locator::Locator {
        let opts = options.map(ferridriver::options::TextOptions::from);
        $crate::locator::Locator::wrap(
          self
            .inner
            .get_by_alt_text($crate::types::getby_input_to_rust(text))
            .maybe_options(opts)
            .into_locator(),
        )
      }

      /// Playwright: `getByTitle(text: string | RegExp, options?): Locator`.
      #[napi(ts_args_type = "text: string | RegExp, options?: TextOptions")]
      pub fn get_by_title(
        &self,
        text: napi::Either<String, $crate::types::JsRegExpLike>,
        options: Option<$crate::types::TextOptions>,
      ) -> $crate::locator::Locator {
        let opts = options.map(ferridriver::options::TextOptions::from);
        $crate::locator::Locator::wrap(
          self
            .inner
            .get_by_title($crate::types::getby_input_to_rust(text))
            .maybe_options(opts)
            .into_locator(),
        )
      }

      /// Playwright: `getByTestId(testId: string | RegExp): Locator`.
      #[napi(ts_args_type = "testId: string | RegExp")]
      pub fn get_by_test_id(
        &self,
        test_id: napi::Either<String, $crate::types::JsRegExpLike>,
      ) -> $crate::locator::Locator {
        $crate::locator::Locator::wrap(self.inner.get_by_test_id($crate::types::getby_input_to_rust(test_id)))
      }
    }
  };
}

/// Generate the shared Playwright selector-action, content-read, state-check,
/// evaluation, tag-injection, and wait methods common to both `Page` and `Frame`.
macro_rules! napi_selector_surface {
  ($target:ident) => {
    #[napi_derive::napi]
    impl $target {
      #[napi]
      pub fn locator(
        &self,
        selector: String,
        options: Option<$crate::types::FilterOptions>,
      ) -> $crate::locator::Locator {
        let opts = options.map(ferridriver::options::FilterOptions::from);
        $crate::locator::Locator::wrap(match opts {
          Some(f) => self.inner.locator_with(&selector, &f),
          None => self.inner.locator(&selector),
        })
      }

      #[napi]
      pub async fn title(&self) -> napi::Result<String> {
        $crate::error::IntoNapi::into_napi(self.inner.title().await)
      }

      #[napi(js_name = "$")]
      pub async fn dollar(
        &self,
        selector: String,
        options: Option<$crate::types::QuerySelectorOptions>,
      ) -> napi::Result<Option<$crate::element_handle::ElementHandle>> {
        let handle = $crate::error::IntoNapi::into_napi(
          self
            .inner
            .query_selector(&selector, options.and_then(|o| o.strict))
            .await,
        )?;
        Ok(handle.map($crate::element_handle::ElementHandle::wrap))
      }

      #[napi(js_name = "$$")]
      pub async fn dollar_dollar(&self, selector: String) -> napi::Result<Vec<$crate::element_handle::ElementHandle>> {
        let handles = $crate::error::IntoNapi::into_napi(self.inner.query_selector_all(&selector).await)?;
        Ok(
          handles
            .into_iter()
            .map($crate::element_handle::ElementHandle::wrap)
            .collect(),
        )
      }

      #[napi(ts_args_type = "options?: { content?: string, path?: string, type?: string, url?: string }")]
      pub async fn add_script_tag(
        &self,
        options: Option<$crate::types::AddScriptTagOptions>,
      ) -> napi::Result<$crate::element_handle::ElementHandle> {
        let handle = $crate::error::IntoNapi::into_napi(
          self
            .inner
            .add_script_tag()
            .maybe_options(options.map(Into::into))
            .await,
        )?;
        Ok($crate::element_handle::ElementHandle::wrap(handle))
      }

      #[napi(ts_args_type = "options?: { content?: string, path?: string, url?: string }")]
      pub async fn add_style_tag(
        &self,
        options: Option<$crate::types::AddStyleTagOptions>,
      ) -> napi::Result<$crate::element_handle::ElementHandle> {
        let handle = $crate::error::IntoNapi::into_napi(
          self
            .inner
            .add_style_tag()
            .maybe_options(options.map(Into::into))
            .await,
        )?;
        Ok($crate::element_handle::ElementHandle::wrap(handle))
      }

      #[napi]
      pub async fn click(&self, selector: String, options: Option<$crate::types::ClickOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.click(&selector), options)?.await)
      }

      #[napi]
      pub async fn dblclick(
        &self,
        selector: String,
        options: Option<$crate::types::DblClickOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.dblclick(&selector), options)?.await)
      }

      #[napi]
      pub async fn hover(&self, selector: String, options: Option<$crate::types::HoverOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.hover(&selector), options)?.await)
      }

      #[napi]
      pub async fn tap(&self, selector: String, options: Option<$crate::types::TapOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.tap(&selector), options)?.await)
      }

      #[napi]
      pub async fn focus(&self, selector: String) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(self.inner.focus(&selector).await)
      }

      #[napi]
      pub async fn fill(
        &self,
        selector: String,
        value: String,
        options: Option<$crate::types::FillOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.fill(&selector, &value), options)?.await)
      }

      #[napi]
      pub async fn press(
        &self,
        selector: String,
        key: String,
        options: Option<$crate::types::PressOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.press(&selector, &key), options)?.await)
      }

      #[napi]
      pub async fn check(&self, selector: String, options: Option<$crate::types::CheckOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.check(&selector), options)?.await)
      }

      #[napi]
      pub async fn uncheck(&self, selector: String, options: Option<$crate::types::CheckOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.uncheck(&selector), options)?.await)
      }

      #[napi]
      pub async fn set_checked(
        &self,
        selector: String,
        checked: bool,
        options: Option<$crate::types::CheckOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(self.inner.set_checked(&selector, checked), options)?.await,
        )
      }

      #[napi]
      pub async fn select_option(
        &self,
        selector: String,
        values: $crate::types::NapiSelectOptionInput,
        options: Option<$crate::types::SelectOptionOptions>,
      ) -> napi::Result<Vec<String>> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(self.inner.select_option(&selector, values.0), options)?.await,
        )
      }

      #[napi(
        ts_args_type = "selector: string, files: string | string[] | FilePayload | FilePayload[], options?: SetInputFilesOptions"
      )]
      pub async fn set_input_files(
        &self,
        selector: String,
        files: $crate::types::NapiInputFiles,
        options: Option<$crate::types::SetInputFilesOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(self.inner.set_input_files(&selector, files.0), options)?.await,
        )
      }

      #[napi]
      pub async fn drag_and_drop(
        &self,
        source: String,
        target: String,
        options: Option<$crate::types::DragAndDropOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(self.inner.drag_and_drop(&source, &target), options)?.await,
        )
      }

      #[napi(ts_args_type = "selector: string, eventType: string, eventInit?: unknown, options?: DispatchEventOptions")]
      pub async fn dispatch_event(
        &self,
        selector: String,
        event_type: String,
        event_init: Option<$crate::types::NapiEvaluateArg>,
        options: Option<$crate::types::DispatchEventOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(
            self.inner.dispatch_event(
              &selector,
              &event_type,
              $crate::page::build_evaluate_argument(event_init, false)?,
            ),
            options,
          )?
          .await,
        )
      }

      #[napi]
      pub async fn text_content(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<Option<String>> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.text_content(&selector), options)?.await)
      }

      #[napi]
      pub async fn inner_text(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<String> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.inner_text(&selector), options)?.await)
      }

      #[napi(js_name = "innerHTML")]
      pub async fn inner_html(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<String> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.inner_html(&selector), options)?.await)
      }

      #[napi]
      pub async fn get_attribute(
        &self,
        selector: String,
        name: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<Option<String>> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(self.inner.get_attribute(&selector, &name), options)?.await,
        )
      }

      #[napi]
      pub async fn input_value(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<String> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.input_value(&selector), options)?.await)
      }

      #[napi]
      pub async fn is_visible(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<bool> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.is_visible(&selector), options)?.await)
      }

      #[napi]
      pub async fn is_hidden(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<bool> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.is_hidden(&selector), options)?.await)
      }

      #[napi]
      pub async fn is_enabled(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<bool> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.is_enabled(&selector), options)?.await)
      }

      #[napi]
      pub async fn is_disabled(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<bool> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.is_disabled(&selector), options)?.await)
      }

      #[napi]
      pub async fn is_checked(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<bool> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.is_checked(&selector), options)?.await)
      }

      #[napi]
      pub async fn is_editable(
        &self,
        selector: String,
        options: Option<$crate::types::SelectorReadOptions>,
      ) -> napi::Result<bool> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.is_editable(&selector), options)?.await)
      }

      #[napi]
      pub async fn wait_for_selector(
        &self,
        selector: String,
        options: Option<$crate::types::WaitOptions>,
      ) -> napi::Result<Option<$crate::element_handle::ElementHandle>> {
        let handle = $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(self.inner.wait_for_selector(&selector), options)?.await,
        )?;
        Ok(handle.map($crate::element_handle::ElementHandle::wrap))
      }

      #[napi(
        js_name = "waitForURL",
        ts_args_type = "url: string | RegExp | ((url: URL) => boolean | Promise<boolean>), options?: { timeout?: number, signal?: AbortSignal, waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }",
        ts_return_type = "Promise<void>"
      )]
      pub fn wait_for_url(
        &self,
        env: napi::Env,
        url: $crate::page::UrlArg<'_>,
        options: Option<$crate::types::WaitForUrlOptions>,
      ) -> napi::Result<napi::bindgen_prelude::AsyncBlock<()>> {
        let wait = $crate::abort::Bag::bag(self.inner.wait_for_url($crate::page::url_condition(url)?), options)?;
        napi::bindgen_prelude::AsyncBlockBuilder::new(async move { $crate::error::IntoNapi::into_napi(wait.await) })
          .build(&env)
      }

      #[napi(
        ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: { polling?: number | 'raf', timeout?: number, signal?: AbortSignal }"
      )]
      pub async fn wait_for_function(
        &self,
        page_function: $crate::types::NapiPageFunction,
        arg: Option<$crate::types::NapiEvaluateArg>,
        mut options: Option<$crate::types::WaitForFunctionOptions>,
      ) -> napi::Result<$crate::js_handle::JSHandle> {
        let serialized = $crate::page::build_serialized_argument(arg)?;
        let signal = $crate::abort::take(&mut options);
        let opts = options
          .map(ferridriver::options::WaitForFunctionOptions::try_from)
          .transpose()?;
        let handle = $crate::abort::run(
          signal,
          self
            .inner
            .wait_for_function_serialized(&page_function.source, serialized, page_function.is_function, opts),
        )
        .await?;
        Ok($crate::js_handle::JSHandle::wrap(handle))
      }

      #[napi(ts_args_type = "options?: { includeShadow?: boolean }")]
      pub async fn content(&self, options: Option<$crate::types::ContentOptions>) -> napi::Result<String> {
        let options = ferridriver::options::ContentOptions {
          include_shadow: options.and_then(|o| o.include_shadow),
        };
        $crate::error::IntoNapi::into_napi(self.inner.content().options(options).await)
      }

      #[napi(
        ts_args_type = "html: string, options?: { timeout?: number, signal?: AbortSignal, waitUntil?: 'load' | 'domcontentloaded' | 'networkidle' | 'commit' }"
      )]
      pub async fn set_content(
        &self,
        html: String,
        options: Option<$crate::types::SetContentOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.set_content(&html), options)?.await)
      }

      #[napi(
        ts_args_type = "state?: 'load' | 'domcontentloaded' | 'networkidle', options?: { timeout?: number, signal?: AbortSignal }"
      )]
      pub async fn wait_for_load_state(
        &self,
        state: Option<String>,
        options: Option<$crate::types::WaitForLoadStateOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(self.inner.wait_for_load_state(state.as_deref()), options)?.await,
        )
      }

      #[napi]
      pub async fn wait_for_timeout(&self, timeout: f64) {
        self.inner.wait_for_timeout($crate::types::f64_to_u64(timeout)).await;
      }

      #[napi(
        ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: ExposeFunctionsOptions",
        ts_return_type = "Promise<unknown>"
      )]
      pub async fn evaluate(
        &self,
        page_function: $crate::types::NapiPageFunction,
        arg: Option<$crate::types::NapiEvaluateArg>,
        options: Option<$crate::types::ExposeFunctionsOptions>,
      ) -> napi::Result<$crate::serialize_out::Evaluated> {
        let serialized =
          $crate::page::build_evaluate_argument(arg, options.and_then(|o| o.expose_functions).unwrap_or(false))?;
        let result = $crate::error::IntoNapi::into_napi(
          self
            .inner
            .evaluate(&page_function.source, serialized, page_function.is_function)
            .await,
        )?;
        Ok($crate::serialize_out::Evaluated(result))
      }

      #[napi(ts_args_type = "pageFunction: string | Function, arg?: unknown, options?: ExposeFunctionsOptions")]
      pub async fn evaluate_handle(
        &self,
        page_function: $crate::types::NapiPageFunction,
        arg: Option<$crate::types::NapiEvaluateArg>,
        options: Option<$crate::types::ExposeFunctionsOptions>,
      ) -> napi::Result<$crate::js_handle::JSHandle> {
        let serialized =
          $crate::page::build_evaluate_argument(arg, options.and_then(|o| o.expose_functions).unwrap_or(false))?;
        let handle = $crate::error::IntoNapi::into_napi(
          self
            .inner
            .evaluate_handle(&page_function.source, serialized, page_function.is_function)
            .await,
        )?;
        Ok($crate::js_handle::JSHandle::wrap(handle))
      }

      #[napi(
        js_name = "$eval",
        ts_args_type = "selector: string, pageFunction: string | Function, arg?: unknown",
        ts_return_type = "Promise<unknown>"
      )]
      pub async fn eval_on_selector(
        &self,
        selector: String,
        page_function: $crate::types::NapiPageFunction,
        arg: Option<$crate::types::NapiEvaluateArg>,
      ) -> napi::Result<$crate::serialize_out::Evaluated> {
        let serialized = $crate::page::build_serialized_argument(arg)?;
        let result = $crate::error::IntoNapi::into_napi(
          self
            .inner
            .eval_on_selector(&selector, &page_function.source, serialized, page_function.is_function)
            .await,
        )?;
        Ok($crate::serialize_out::Evaluated(result))
      }

      #[napi(
        js_name = "$$eval",
        ts_args_type = "selector: string, pageFunction: string | Function, arg?: unknown",
        ts_return_type = "Promise<unknown>"
      )]
      pub async fn eval_on_selector_all(
        &self,
        selector: String,
        page_function: $crate::types::NapiPageFunction,
        arg: Option<$crate::types::NapiEvaluateArg>,
      ) -> napi::Result<$crate::serialize_out::Evaluated> {
        let serialized = $crate::page::build_serialized_argument(arg)?;
        let result = $crate::error::IntoNapi::into_napi(
          self
            .inner
            .eval_on_selector_all(&selector, &page_function.source, serialized, page_function.is_function)
            .await,
        )?;
        Ok($crate::serialize_out::Evaluated(result))
      }
    }
  };
}

/// Generate the shared element-targeted action, geometry, and screenshot
/// methods common to `Locator` and `ElementHandle`.
macro_rules! napi_element_actions {
  ($target:ident) => {
    #[napi_derive::napi]
    impl $target {
      #[napi]
      pub async fn click(&self, options: Option<$crate::types::ClickOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.click(), options)?.await)
      }

      #[napi]
      pub async fn dblclick(&self, options: Option<$crate::types::DblClickOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.dblclick(), options)?.await)
      }

      #[napi]
      pub async fn hover(&self, options: Option<$crate::types::HoverOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.hover(), options)?.await)
      }

      #[napi]
      pub async fn tap(&self, options: Option<$crate::types::TapOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.tap(), options)?.await)
      }

      #[napi]
      pub async fn focus(&self) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(self.inner.focus().await)
      }

      #[napi]
      pub async fn fill(&self, value: String, options: Option<$crate::types::FillOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.fill(&value), options)?.await)
      }

      #[napi]
      pub async fn press(&self, key: String, options: Option<$crate::types::PressOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.press(&key), options)?.await)
      }

      #[napi]
      pub async fn check(&self, options: Option<$crate::types::CheckOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.check(), options)?.await)
      }

      #[napi]
      pub async fn uncheck(&self, options: Option<$crate::types::CheckOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.uncheck(), options)?.await)
      }

      #[napi]
      pub async fn set_checked(&self, checked: bool, options: Option<$crate::types::CheckOptions>) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.set_checked(checked), options)?.await)
      }

      #[napi]
      pub async fn select_option(
        &self,
        values: $crate::types::NapiSelectOptionInput,
        options: Option<$crate::types::SelectOptionOptions>,
      ) -> napi::Result<Vec<String>> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.select_option(values.0), options)?.await)
      }

      #[napi]
      pub async fn select_text(&self) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(self.inner.select_text().await)
      }

      #[napi(ts_args_type = "files: string | string[] | FilePayload | FilePayload[], options?: SetInputFilesOptions")]
      pub async fn set_input_files(
        &self,
        files: $crate::types::NapiInputFiles,
        options: Option<$crate::types::SetInputFilesOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi($crate::abort::Bag::bag(self.inner.set_input_files(files.0), options)?.await)
      }

      #[napi(ts_args_type = "eventType: string, eventInit?: unknown, options?: DispatchEventOptions")]
      pub async fn dispatch_event(
        &self,
        event_type: String,
        event_init: Option<$crate::types::NapiEvaluateArg>,
        options: Option<$crate::types::DispatchEventOptions>,
      ) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(
          $crate::abort::Bag::bag(
            self
              .inner
              .dispatch_event(&event_type, $crate::page::build_evaluate_argument(event_init, false)?),
            options,
          )?
          .await,
        )
      }

      #[napi(js_name = "scrollIntoViewIfNeeded")]
      pub async fn scroll_into_view_if_needed(&self) -> napi::Result<()> {
        $crate::error::IntoNapi::into_napi(self.inner.scroll_into_view_if_needed().await)
      }

      #[napi]
      pub async fn bounding_box(&self) -> napi::Result<Option<$crate::types::BoundingBox>> {
        Ok($crate::error::IntoNapi::into_napi(self.inner.bounding_box().await)?.map(Into::into))
      }

      #[napi]
      pub async fn screenshot(
        &self,
        mut options: Option<$crate::types::ElementScreenshotOptions>,
      ) -> napi::Result<napi::bindgen_prelude::Buffer> {
        let signal = $crate::abort::take(&mut options);
        let opts = options.unwrap_or_default().into_core(self.inner.page());
        let bytes =
          $crate::error::IntoNapi::into_napi(self.inner.screenshot().options(opts).maybe_signal(signal).await)?;
        Ok(bytes.into())
      }
    }
  };
}

/// Generate the Node `EventEmitter` methods (`on`, `addListener`, `once`,
/// `prependListener`, `prependOnceListener`, `off`, `removeListener`,
/// `removeAllListeners`, `listenerCount`, `eventNames`, `setMaxListeners`,
/// `getMaxListeners`) backed by [`crate::emitter::ListenerRegistry`].
macro_rules! napi_event_emitter {
  (
    target: $target:ident,
    arg: $arg:ty,
    ts_on: $ts_on:literal,
    ts_off: $ts_off:literal,
    full
  ) => {
    #[napi_derive::napi]
    impl $target {
      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn on<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.register_listener(&event, &listener, false, false)?;
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn add_listener<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.on(this, event, listener)
      }

      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn once<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.register_listener(&event, &listener, true, false)?;
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn prepend_listener<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.register_listener(&event, &listener, false, true)?;
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn prepend_once_listener<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.register_listener(&event, &listener, true, true)?;
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_off, ts_return_type = "this")]
      pub fn off<'env>(
        &self,
        env: napi::Env,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: Option<napi::bindgen_prelude::Function<'_, $arg, ()>>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        match listener {
          None => {
            self.inner.remove_listeners_named(&event);
            self.listener_regs.forget_event(&event);
          },
          Some(listener) => {
            for id in self.listener_regs.take_matching(env, &event, &listener)? {
              self.inner.off(ferridriver::events::ListenerId(id));
            }
          },
        }
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_off, ts_return_type = "this")]
      pub fn remove_listener<'env>(
        &self,
        env: napi::Env,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: Option<napi::bindgen_prelude::Function<'_, $arg, ()>>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.off(env, this, event, listener)
      }

      #[napi(ts_return_type = "this")]
      pub fn remove_all_listeners<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: Option<String>,
      ) -> napi::bindgen_prelude::Object<'env> {
        if let Some(ev) = event {
          self.inner.remove_listeners_named(&ev);
          self.listener_regs.forget_event(&ev);
        } else {
          self.inner.remove_all_listeners();
          self.listener_regs.clear();
        }
        this.object
      }

      #[napi]
      pub fn listener_count(&self, event: String) -> f64 {
        $crate::types::usize_to_f64(self.inner.listener_count(&event))
      }

      #[napi]
      pub fn event_names(&self) -> Vec<String> {
        self.inner.event_names()
      }

      #[napi(ts_return_type = "this")]
      pub fn set_max_listeners<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        max: f64,
      ) -> napi::bindgen_prelude::Object<'env> {
        self.inner.set_max_listeners($crate::types::f64_to_usize(max));
        this.object
      }

      #[napi]
      pub fn get_max_listeners(&self) -> f64 {
        $crate::types::usize_to_f64(self.inner.max_listeners())
      }
    }
  };
  (
    target: $target:ident,
    arg: $arg:ty,
    ts_on: $ts_on:literal,
    ts_off: $ts_off:literal,
    basic_events
  ) => {
    #[napi_derive::napi]
    impl $target {
      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn on<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.register_listener(&event, &listener, false)?;
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn add_listener<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.on(this, event, listener)
      }

      #[napi(ts_args_type = $ts_on, ts_return_type = "this")]
      pub fn once<'env>(
        &self,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: napi::bindgen_prelude::Function<'_, $arg, ()>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.register_listener(&event, &listener, true)?;
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_off, ts_return_type = "this")]
      pub fn off<'env>(
        &self,
        env: napi::Env,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: Option<napi::bindgen_prelude::Function<'_, $arg, ()>>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        match listener {
          None => {
            self.inner.events().remove_listeners_named(&event);
            self.listener_regs.forget_event(&event);
          },
          Some(listener) => {
            for id in self.listener_regs.take_matching(env, &event, &listener)? {
              self.inner.events().off(ferridriver::events::ListenerId(id));
            }
          },
        }
        Ok(this.object)
      }

      #[napi(ts_args_type = $ts_off, ts_return_type = "this")]
      pub fn remove_listener<'env>(
        &self,
        env: napi::Env,
        this: napi::bindgen_prelude::This<'env>,
        event: String,
        listener: Option<napi::bindgen_prelude::Function<'_, $arg, ()>>,
      ) -> napi::Result<napi::bindgen_prelude::Object<'env>> {
        self.off(env, this, event, listener)
      }
    }
  };
}

pub(crate) use napi_element_actions;
pub(crate) use napi_event_emitter;
pub(crate) use napi_locator_queries;
pub(crate) use napi_selector_surface;
