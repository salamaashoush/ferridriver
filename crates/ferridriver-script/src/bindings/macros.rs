//! Shared declarative mixins for `rquickjs::methods` impl blocks across
//! `PageJs`, `FrameJs`, `LocatorJs`, `FrameLocatorJs`, `ElementHandleJs`,
//! `BrowserContextJs`, and `BrowserJs`.
//!
//! `rquickjs` allows only one `#[rquickjs::methods] impl` block per class.
//! [`qjs_impl!`] expands each declared `@mixin` into plain method items
//! before emitting the single `#[rquickjs::methods] impl`, removing ~3,000
//! lines of 2x–4x copy-pasted bindings with zero runtime or proc-macro cost.

/// Expand shared mixins and class-specific methods into a single
/// `#[rquickjs::methods] impl` block.
macro_rules! qjs_impl {
  (
    $( #[$attr:meta] )*
    impl $target:ident {
      $($body:tt)*
    }
  ) => {
    $crate::bindings::macros::qjs_impl!(@parse [ $( #[$attr] )* ] $target [] [ $($body)* ]);
  };

  (@parse $attrs:tt $target:ident [ $($mixins:tt)* ] [ @mixin $mixin:ident ( $($args:tt)* ); $($rest:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@parse $attrs $target [ $($mixins)* $mixin ( $($args)* ), ] [ $($rest)* ]);
  };

  (@parse $attrs:tt $target:ident [ $($mixins:tt)* ] [ $($rest:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@step $attrs $target [ $($mixins)* ] [ $($rest)* ]);
  };

  (@step [ $( #[$attr:meta] )* ] $target:ident [] [ $($items:tt)* ]) => {
    #[rquickjs::methods]
    $( #[$attr] )*
    impl $target {
      $($items)*
    }
  };

  // ── Mixin: locator_queries(optional_frame_locator) ────────────────────
  (@step $attrs:tt $target:ident [ locator_queries(optional_frame_locator), $($m:tt)* ] [ $($items:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@step $attrs $target [ locator_queries(common), $($m)* ] [
      /// Playwright: `frameLocator(selector?: string): FrameLocator`.
      #[qjs(rename = "frameLocator")]
      #[must_use]
      pub fn frame_locator(
        &self,
        selector: rquickjs::function::Opt<String>,
      ) -> $crate::bindings::frame_locator::FrameLocatorJs {
        $crate::bindings::frame_locator::FrameLocatorJs::new(self.inner.frame_locator(selector.0.as_deref()))
      }

      $($items)*
    ]);
  };

  // ── Mixin: locator_queries(required_frame_locator) ────────────────────
  (@step $attrs:tt $target:ident [ locator_queries(required_frame_locator), $($m:tt)* ] [ $($items:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@step $attrs $target [ locator_queries(common), $($m)* ] [
      /// Playwright: `frameLocator(selector: string): FrameLocator`.
      #[qjs(rename = "frameLocator")]
      #[must_use]
      pub fn frame_locator(&self, selector: String) -> $crate::bindings::frame_locator::FrameLocatorJs {
        $crate::bindings::frame_locator::FrameLocatorJs::new(self.inner.frame_locator(selector.as_str()))
      }

      $($items)*
    ]);
  };

  // ── Internal: common getBy* locator builders ──────────────────────────
  (@step $attrs:tt $target:ident [ locator_queries(common), $($m:tt)* ] [ $($items:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@step $attrs $target [ $($m)* ] [
      /// Playwright: `getByRole(role, options?)`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `options.name` or `options.description` is neither a string nor a `RegExp`.
      #[qjs(rename = "getByRole")]
      pub fn get_by_role(
        &self,
        role: String,
        options: rquickjs::function::Opt<rquickjs::Value<'_>>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let opts = $crate::bindings::page::parse_role_options(options)?;
        Ok($crate::bindings::locator::LocatorJs::new(
          self.inner.get_by_role(role.as_str()).options(opts).into_locator(),
        ))
      }

      /// Playwright: `getByText(text, options?)`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
      #[qjs(rename = "getByText")]
      pub fn get_by_text(
        &self,
        text: rquickjs::Value<'_>,
        options: rquickjs::function::Opt<rquickjs::Value<'_>>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let t = $crate::bindings::page::string_or_regex_from_js(&text)?;
        let opts = $crate::bindings::page::parse_text_options(options);
        Ok($crate::bindings::locator::LocatorJs::new(
          self.inner.get_by_text(t).options(opts).into_locator(),
        ))
      }

      /// Playwright: `getByLabel(text, options?)`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
      #[qjs(rename = "getByLabel")]
      pub fn get_by_label(
        &self,
        text: rquickjs::Value<'_>,
        options: rquickjs::function::Opt<rquickjs::Value<'_>>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let t = $crate::bindings::page::string_or_regex_from_js(&text)?;
        let opts = $crate::bindings::page::parse_text_options(options);
        Ok($crate::bindings::locator::LocatorJs::new(
          self.inner.get_by_label(t).options(opts).into_locator(),
        ))
      }

      /// Playwright: `getByPlaceholder(text, options?)`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
      #[qjs(rename = "getByPlaceholder")]
      pub fn get_by_placeholder(
        &self,
        text: rquickjs::Value<'_>,
        options: rquickjs::function::Opt<rquickjs::Value<'_>>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let t = $crate::bindings::page::string_or_regex_from_js(&text)?;
        let opts = $crate::bindings::page::parse_text_options(options);
        Ok($crate::bindings::locator::LocatorJs::new(
          self.inner.get_by_placeholder(t).options(opts).into_locator(),
        ))
      }

      /// Playwright: `getByAltText(text, options?)`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
      #[qjs(rename = "getByAltText")]
      pub fn get_by_alt_text(
        &self,
        text: rquickjs::Value<'_>,
        options: rquickjs::function::Opt<rquickjs::Value<'_>>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let t = $crate::bindings::page::string_or_regex_from_js(&text)?;
        let opts = $crate::bindings::page::parse_text_options(options);
        Ok($crate::bindings::locator::LocatorJs::new(
          self.inner.get_by_alt_text(t).options(opts).into_locator(),
        ))
      }

      /// Playwright: `getByTitle(text, options?)`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `text` is neither a string nor a `RegExp`.
      #[qjs(rename = "getByTitle")]
      pub fn get_by_title(
        &self,
        text: rquickjs::Value<'_>,
        options: rquickjs::function::Opt<rquickjs::Value<'_>>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let t = $crate::bindings::page::string_or_regex_from_js(&text)?;
        let opts = $crate::bindings::page::parse_text_options(options);
        Ok($crate::bindings::locator::LocatorJs::new(
          self.inner.get_by_title(t).options(opts).into_locator(),
        ))
      }

      /// Playwright: `getByTestId(testId)`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `testId` is neither a string nor a `RegExp`.
      #[qjs(rename = "getByTestId")]
      pub fn get_by_test_id(
        &self,
        test_id: rquickjs::Value<'_>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let t = $crate::bindings::page::string_or_regex_from_js(&test_id)?;
        Ok($crate::bindings::locator::LocatorJs::new(self.inner.get_by_test_id(t)))
      }

      $($items)*
    ]);
  };

  // ── Mixin: selector_surface() (shared by PageJs and FrameJs) ──────────
  // Requires `self.backend_page_id() -> u64` on `$target`.
  (@step $attrs:tt $target:ident [ selector_surface(), $($m:tt)* ] [ $($items:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@step $attrs $target [ $($m)* ] [
      /// Playwright: `locator(selector, options?: LocatorOptions): Locator`.
      ///
      /// # Errors
      ///
      /// Throws a `TypeError` when `options` is malformed.
      #[qjs(rename = "locator")]
      pub fn locator<'js>(
        &self,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::locator::LocatorJs> {
        let parsed = $crate::bindings::locator::parse_locator_options_public(&ctx, options, true)?;
        let opts = ferridriver::options::FilterOptions {
          has_text: parsed.has_text,
          has_not_text: parsed.has_not_text,
          has: parsed.has,
          has_not: parsed.has_not,
          visible: parsed.visible,
        };
        let locator = if $crate::bindings::locator::is_empty_filter(&opts) {
          self.inner.locator(&selector)
        } else {
          self.inner.locator_with(&selector, &opts)
        };
        Ok($crate::bindings::locator::LocatorJs::new(locator))
      }

      /// Document title.
      ///
      /// # Errors
      ///
      /// Rejects with the core error when reading the title fails.
      #[qjs(rename = "title")]
      pub async fn title(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'_>,
      ) -> rquickjs::Result<String> {
        call_site
          .scope(async move { $crate::bindings::convert::FerriResultCtxExt::into_js_with(self.inner.title().await, &ctx) })
          .await
      }

      /// Full HTML content.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "content")]
      pub async fn content<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<String> {
        let include_shadow = match options.0.as_ref().and_then(rquickjs::Value::as_object) {
          Some(bag) => bag.get::<_, Option<bool>>("includeShadow")?,
          None => None,
        };
        let options = ferridriver::options::ContentOptions { include_shadow };
        call_site
          .scope(async move {
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.content().options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `setContent(html, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "setContent")]
      pub async fn set_content<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        html: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts: Option<ferridriver::options::SetContentOptions> =
              $crate::bindings::convert::parse_opt_bag(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.set_content(&html).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `addScriptTag(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError`, `PermissionDeniedError`, or the core error.
      #[qjs(rename = "addScriptTag")]
      pub async fn add_script_tag<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::element_handle::ElementHandleJs> {
        call_site
          .scope(async move {
            let opts: Option<ferridriver::options::AddScriptTagOptions> =
              $crate::bindings::convert::parse_opt_bag(&ctx, options)?;
            if let Some(path) = opts.as_ref().and_then(|o| o.path.as_deref()) {
              $crate::bindings::convert::readable(&ctx, path)?;
            }
            let handle = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.add_script_tag().maybe_options(opts).await,
              &ctx,
            )?;
            Ok($crate::bindings::element_handle::ElementHandleJs::new(handle))
          })
          .await
      }

      /// Playwright: `addStyleTag(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError`, `PermissionDeniedError`, or the core error.
      #[qjs(rename = "addStyleTag")]
      pub async fn add_style_tag<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::element_handle::ElementHandleJs> {
        call_site
          .scope(async move {
            let opts: Option<ferridriver::options::AddStyleTagOptions> =
              $crate::bindings::convert::parse_opt_bag(&ctx, options)?;
            if let Some(path) = opts.as_ref().and_then(|o| o.path.as_deref()) {
              $crate::bindings::convert::readable(&ctx, path)?;
            }
            let handle = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.add_style_tag().maybe_options(opts).await,
              &ctx,
            )?;
            Ok($crate::bindings::element_handle::ElementHandleJs::new(handle))
          })
          .await
      }

      /// Playwright: `$(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error.
      #[qjs(rename = "$")]
      pub async fn dollar<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::convert::Null<$crate::bindings::element_handle::ElementHandleJs>> {
        call_site
          .scope(async move {
            let strict = $crate::bindings::page::opt_strict(&options)?;
            let handle = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.query_selector(&selector, strict).await,
              &ctx,
            )?;
            Ok(handle.map($crate::bindings::element_handle::ElementHandleJs::new))
          })
          .await
          .map($crate::bindings::convert::Null)
      }

      /// Playwright: `$$(selector)`.
      ///
      /// # Errors
      ///
      /// Rejects with the core error when the query fails.
      #[qjs(rename = "$$")]
      pub async fn dollar_dollar(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'_>,
        selector: String,
      ) -> rquickjs::Result<Vec<$crate::bindings::element_handle::ElementHandleJs>> {
        call_site
          .scope(async move {
            let handles = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.query_selector_all(&selector).await,
              &ctx,
            )?;
            Ok(
              handles
                .into_iter()
                .map($crate::bindings::element_handle::ElementHandleJs::new)
                .collect(),
            )
          })
          .await
      }

      /// Playwright: `evaluate(pageFunction, arg?, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when `pageFunction`, `arg`, or `options` cannot be converted, or on evaluation failure.
      #[qjs(rename = "evaluate")]
      pub async fn evaluate<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        page_function: rquickjs::Value<'js>,
        arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<rquickjs::Value<'js>> {
        call_site
          .scope(async move {
            let (source, is_fn) = $crate::bindings::convert::extract_page_function(&ctx, page_function)?;
            let serialized = $crate::bindings::convert::quickjs_arg_with_functions(
              &ctx,
              arg.0,
              $crate::bindings::convert::ArgFunctions::from_options(
                options.0.as_ref(),
                Some(self.backend_page_id()),
              )?,
            )?;
            let result = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.evaluate(&source, serialized, is_fn).await,
              &ctx,
            )?;
            $crate::bindings::convert::serialized_value_to_quickjs(&ctx, &result)
          })
          .await
      }

      /// Playwright: `evaluateHandle(pageFunction, arg?, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when `pageFunction`, `arg`, or `options` cannot be converted, or on evaluation failure.
      #[qjs(rename = "evaluateHandle")]
      pub async fn evaluate_handle<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        page_function: rquickjs::Value<'js>,
        arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::js_handle::JSHandleJs> {
        call_site
          .scope(async move {
            let (source, is_fn) = $crate::bindings::convert::extract_page_function(&ctx, page_function)?;
            let serialized = $crate::bindings::convert::quickjs_arg_with_functions(
              &ctx,
              arg.0,
              $crate::bindings::convert::ArgFunctions::from_options(
                options.0.as_ref(),
                Some(self.backend_page_id()),
              )?,
            )?;
            let handle = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.evaluate_handle(&source, serialized, is_fn).await,
              &ctx,
            )?;
            Ok($crate::bindings::js_handle::JSHandleJs::new(handle))
          })
          .await
      }

      /// Playwright: `$eval(selector, pageFunction, arg?)`.
      ///
      /// # Errors
      ///
      /// Rejects when `pageFunction` or `arg` cannot be converted, or on evaluation failure.
      #[qjs(rename = "$eval")]
      pub async fn eval_on_selector<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        page_function: rquickjs::Value<'js>,
        arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<rquickjs::Value<'js>> {
        call_site
          .scope(async move {
            let (source, is_fn) = $crate::bindings::convert::extract_page_function(&ctx, page_function)?;
            let serialized = $crate::bindings::convert::quickjs_arg_to_serialized(&ctx, arg.0)?;
            let result = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.eval_on_selector(&selector, &source, serialized, is_fn).await,
              &ctx,
            )?;
            $crate::bindings::convert::serialized_value_to_quickjs(&ctx, &result)
          })
          .await
      }

      /// Playwright: `$$eval(selector, pageFunction, arg?)`.
      ///
      /// # Errors
      ///
      /// Rejects when `pageFunction` or `arg` cannot be converted, or on evaluation failure.
      #[qjs(rename = "$$eval")]
      pub async fn eval_on_selector_all<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        page_function: rquickjs::Value<'js>,
        arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<rquickjs::Value<'js>> {
        call_site
          .scope(async move {
            let (source, is_fn) = $crate::bindings::convert::extract_page_function(&ctx, page_function)?;
            let serialized = $crate::bindings::convert::quickjs_arg_to_serialized(&ctx, arg.0)?;
            let result = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.eval_on_selector_all(&selector, &source, serialized, is_fn).await,
              &ctx,
            )?;
            $crate::bindings::convert::serialized_value_to_quickjs(&ctx, &result)
          })
          .await
      }

      /// Playwright: `waitForLoadState(state?, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "waitForLoadState")]
      pub async fn wait_for_load_state<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        state: rquickjs::function::Opt<Option<String>>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts: Option<ferridriver::options::WaitForLoadStateOptions> =
              $crate::bindings::convert::parse_opt_bag(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self
                .inner
                .wait_for_load_state(state.0.flatten().as_deref())
                .maybe_options(opts)
                .await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `waitForURL(url, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options or matcher, or the core error.
      #[qjs(rename = "waitForURL")]
      pub async fn wait_for_url<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        url: rquickjs::Value<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts: Option<ferridriver::options::WaitForUrlOptions> =
              $crate::bindings::convert::parse_opt_bag(&ctx, options)?;
            $crate::bindings::page::wait_for_url_condition(&ctx, &url, |condition| {
              self.inner.wait_for_url(condition).maybe_options(opts).into_future()
            })
            .await
          })
          .await
      }

      /// Playwright: `waitForFunction(pageFunction, arg?, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when `pageFunction`, `arg`, or `options` cannot be converted, or on timeout.
      #[qjs(rename = "waitForFunction")]
      pub async fn wait_for_function<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        page_function: rquickjs::Value<'js>,
        arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::js_handle::JSHandleJs> {
        call_site
          .scope(async move {
            let opts: Option<ferridriver::options::WaitForFunctionOptions> =
              $crate::bindings::convert::parse_opt_bag(&ctx, options)?;
            let (source, is_function) = $crate::bindings::convert::extract_page_function(&ctx, page_function)?;
            let serialized = $crate::bindings::convert::quickjs_arg_to_serialized(&ctx, arg.0)?;
            let handle = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self
                .inner
                .wait_for_function_serialized(&source, serialized, is_function, opts)
                .await,
              &ctx,
            )?;
            Ok($crate::bindings::js_handle::JSHandleJs::new(handle))
          })
          .await
      }

      /// Playwright: `waitForTimeout(timeout)`.
      #[qjs(rename = "waitForTimeout")]
      pub async fn wait_for_timeout(&self, call_site: $crate::bindings::CallSite, timeout: f64) {
        call_site
          .scope(async move {
            self
              .inner
              .wait_for_timeout($crate::bindings::convert::f64_to_u64(timeout))
              .await;
          })
          .await;
      }

      /// Playwright: `waitForSelector(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on timeout.
      #[qjs(rename = "waitForSelector")]
      pub async fn wait_for_selector<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::convert::Null<$crate::bindings::element_handle::ElementHandleJs>> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::page::parse_wait_options(&ctx, options)?;
            let handle = $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.wait_for_selector(&selector).options(opts).await,
              &ctx,
            )?;
            Ok(handle.map($crate::bindings::element_handle::ElementHandleJs::new))
          })
          .await
          .map($crate::bindings::convert::Null)
      }

      /// Playwright: `click(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "click")]
      pub async fn click<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_click_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.click(&selector).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `dblclick(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "dblclick")]
      pub async fn dblclick<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.dblclick(&selector).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `hover(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "hover")]
      pub async fn hover<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_hover_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.hover(&selector).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `tap(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "tap")]
      pub async fn tap<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_tap_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.tap(&selector).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `focus(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with the core error when focusing fails.
      #[qjs(rename = "focus")]
      pub async fn focus(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'_>,
        selector: String,
        _options: rquickjs::function::Opt<rquickjs::Value<'_>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(self.inner.focus(&selector).await, &ctx)
          })
          .await
      }

      /// Playwright: `fill(selector, value, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "fill")]
      pub async fn fill<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        value: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_fill_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.fill(&selector, &value).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `type(selector, text, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "type")]
      pub async fn type_<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        text: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_type_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.r#type(&selector, &text).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `press(selector, key, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "press")]
      pub async fn press<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        key: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_press_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.press(&selector, &key).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `check(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "check")]
      pub async fn check<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_check_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.check(&selector).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `uncheck(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "uncheck")]
      pub async fn uncheck<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_check_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.uncheck(&selector).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `setChecked(selector, checked, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "setChecked")]
      pub async fn set_checked<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        checked: bool,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_check_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.set_checked(&selector, checked).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `selectOption(selector, values, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed values or options, or the core error.
      #[qjs(rename = "selectOption")]
      pub async fn select_option<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        values: rquickjs::Value<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<Vec<String>> {
        call_site
          .scope(async move {
            let values = $crate::bindings::convert::parse_select_option_values(&ctx, values)?;
            let opts = $crate::bindings::convert::parse_select_option_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.select_option(&selector, values).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `setInputFiles(selector, files, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError`, `PermissionDeniedError`, or the core error.
      #[qjs(rename = "setInputFiles")]
      pub async fn set_input_files<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        files: rquickjs::Value<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let files = $crate::bindings::convert::parse_input_files(&ctx, files)?;
            let opts = $crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.set_input_files(&selector, files).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `dragAndDrop(source, target, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "dragAndDrop")]
      pub async fn drag_and_drop<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        source: String,
        target: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts: Option<ferridriver::options::DragAndDropOptions> =
              $crate::bindings::convert::parse_opt_bag(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.drag_and_drop(&source, &target).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `dispatchEvent(selector, type, eventInit?, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed `eventInit` or options, or the core error.
      #[qjs(rename = "dispatchEvent")]
      pub async fn dispatch_event<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        event_type: String,
        event_init: rquickjs::function::Opt<rquickjs::Value<'js>>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let init_json = $crate::bindings::convert::quickjs_arg_to_serialized(&ctx, event_init.0)?;
            let opts = $crate::bindings::convert::parse_dispatch_event_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self
                .inner
                .dispatch_event(&selector, &event_type, init_json)
                .maybe_options(opts)
                .await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `textContent(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "textContent")]
      pub async fn text_content<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::convert::Null<String>> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.text_content(&selector).options(options).await,
              &ctx,
            )
          })
          .await
          .map($crate::bindings::convert::Null)
      }

      /// Playwright: `innerText(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "innerText")]
      pub async fn inner_text<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<String> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.inner_text(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `innerHTML(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "innerHTML")]
      pub async fn inner_html<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<String> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.inner_html(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `getAttribute(selector, name, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "getAttribute")]
      pub async fn get_attribute<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        name: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<$crate::bindings::convert::Null<String>> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.get_attribute(&selector, &name).options(options).await,
              &ctx,
            )
          })
          .await
          .map($crate::bindings::convert::Null)
      }

      /// Playwright: `inputValue(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "inputValue")]
      pub async fn input_value<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<String> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.input_value(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `isVisible(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "isVisible")]
      pub async fn is_visible<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<bool> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.is_visible(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `isHidden(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "isHidden")]
      pub async fn is_hidden<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<bool> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.is_hidden(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `isEnabled(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "isEnabled")]
      pub async fn is_enabled<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<bool> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.is_enabled(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `isDisabled(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "isDisabled")]
      pub async fn is_disabled<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<bool> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.is_disabled(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `isChecked(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "isChecked")]
      pub async fn is_checked<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<bool> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.is_checked(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `isEditable(selector, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects when reading options throws or the core call fails.
      #[qjs(rename = "isEditable")]
      pub async fn is_editable<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        selector: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<bool> {
        call_site
          .scope(async move {
            let options = $crate::bindings::page::options::selector_read_options(&options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.is_editable(&selector).options(options).await,
              &ctx,
            )
          })
          .await
      }

      $($items)*
    ]);
  };

  // ── Mixin: element_actions() (shared by LocatorJs and ElementHandleJs) ─
  (@step $attrs:tt $target:ident [ element_actions(), $($m:tt)* ] [ $($items:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@step $attrs $target [ $($m)* ] [
      /// Playwright: `click(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "click")]
      pub async fn click<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_click_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.click().maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `dblclick(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "dblclick")]
      pub async fn dblclick<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_dblclick_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.dblclick().maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `hover(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "hover")]
      pub async fn hover<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_hover_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.hover().maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `tap(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "tap")]
      pub async fn tap<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_tap_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.tap().maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `focus()`.
      ///
      /// # Errors
      ///
      /// Rejects with the core error when focusing fails.
      #[qjs(rename = "focus")]
      pub async fn focus(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'_>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(self.inner.focus().await, &ctx)
          })
          .await
      }

      /// Playwright: `fill(value, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "fill")]
      pub async fn fill<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        value: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_fill_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.fill(&value).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `press(key, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "press")]
      pub async fn press<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        key: String,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_press_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.press(&key).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `check(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "check")]
      pub async fn check<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_check_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.check().maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `uncheck(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "uncheck")]
      pub async fn uncheck<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_check_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.uncheck().maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `setChecked(checked, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "setChecked")]
      pub async fn set_checked<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        checked: bool,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::convert::parse_check_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.set_checked(checked).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `selectOption(values, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed values or options, or the core error.
      #[qjs(rename = "selectOption")]
      pub async fn select_option<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        values: rquickjs::Value<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<Vec<String>> {
        call_site
          .scope(async move {
            let values = $crate::bindings::convert::parse_select_option_values(&ctx, values)?;
            let opts = $crate::bindings::convert::parse_select_option_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.select_option(values).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `selectText(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with the core error when selecting text fails.
      #[qjs(rename = "selectText")]
      pub async fn select_text(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'_>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(self.inner.select_text().await, &ctx)
          })
          .await
      }

      /// Playwright: `setInputFiles(files, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError`, `PermissionDeniedError`, or the core error.
      #[qjs(rename = "setInputFiles")]
      pub async fn set_input_files<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        files: rquickjs::Value<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let files = $crate::bindings::convert::parse_input_files(&ctx, files)?;
            let opts = $crate::bindings::convert::parse_set_input_files_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.set_input_files(files).maybe_options(opts).await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `dispatchEvent(type, eventInit?, options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed `eventInit` or options, or the core error.
      #[qjs(rename = "dispatchEvent")]
      pub async fn dispatch_event<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        event_type: String,
        event_init: rquickjs::function::Opt<rquickjs::Value<'js>>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            let init_json = $crate::bindings::convert::quickjs_arg_to_serialized(&ctx, event_init.0)?;
            let opts = $crate::bindings::convert::parse_dispatch_event_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self
                .inner
                .dispatch_event(&event_type, init_json)
                .maybe_options(opts)
                .await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `scrollIntoViewIfNeeded()`.
      ///
      /// # Errors
      ///
      /// Rejects with the core error when scrolling fails.
      #[qjs(rename = "scrollIntoViewIfNeeded")]
      pub async fn scroll_into_view_if_needed(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'_>,
      ) -> rquickjs::Result<()> {
        call_site
          .scope(async move {
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.scroll_into_view_if_needed().await,
              &ctx,
            )
          })
          .await
      }

      /// Playwright: `boundingBox()`. Returns `{ x, y, width, height }` or `null`.
      ///
      /// # Errors
      ///
      /// Rejects with the core error when measuring the element fails.
      #[qjs(rename = "boundingBox")]
      pub async fn bounding_box<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
      ) -> rquickjs::Result<rquickjs::Value<'js>> {
        call_site
          .scope(async move {
            match $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.bounding_box().await,
              &ctx,
            )? {
              None => Ok(rquickjs::Value::new_null(ctx)),
              Some(b) => {
                let obj = rquickjs::Object::new(ctx.clone())?;
                obj.set("x", b.x)?;
                obj.set("y", b.y)?;
                obj.set("width", b.width)?;
                obj.set("height", b.height)?;
                Ok(obj.into_value())
              },
            }
          })
          .await
      }

      /// Playwright: `screenshot(options?)`.
      ///
      /// # Errors
      ///
      /// Rejects with a `TypeError` for malformed options, or the core error on failure.
      #[qjs(rename = "screenshot")]
      pub async fn screenshot<'js>(
        &self,
        call_site: $crate::bindings::CallSite,
        ctx: rquickjs::Ctx<'js>,
        options: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<Vec<u8>> {
        call_site
          .scope(async move {
            let opts = $crate::bindings::page::options::parse_element_screenshot_options(&ctx, options)?;
            $crate::bindings::convert::FerriResultCtxExt::into_js_with(
              self.inner.screenshot().options(opts).await,
              &ctx,
            )
          })
          .await
      }

      $($items)*
    ]);
  };

  // ── Mixin: event_emitter() (shared by PageJs, BrowserContextJs, BrowserJs) ─
  // Requires `register_listener(&self, ctx, event, listener, once, front) -> Result<()>`
  // and `off_impl(&self, ctx, event, listener) -> Result<()>` on `$target`.
  (@step $attrs:tt $target:ident [ event_emitter(), $($m:tt)* ] [ $($items:tt)* ]) => {
    $crate::bindings::macros::qjs_impl!(@step $attrs $target [ $($m)* ] [
      /// Playwright/Node: `on(event, listener)`. Returns `this` for chaining.
      ///
      /// # Errors
      ///
      /// Throws an `Error` when the listener registry is unavailable.
      #[qjs(rename = "on")]
      pub fn on<'js>(
        this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
        ctx: rquickjs::Ctx<'js>,
        event: String,
        listener: rquickjs::Function<'js>,
      ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
        this.0.borrow().register_listener(&ctx, &event, listener, false, false)?;
        Ok(this.0)
      }

      /// Node's `addListener`, an alias of [`Self::on`].
      ///
      /// # Errors
      ///
      /// Throws an `Error` when the listener registry is unavailable.
      #[qjs(rename = "addListener")]
      pub fn add_listener<'js>(
        this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
        ctx: rquickjs::Ctx<'js>,
        event: String,
        listener: rquickjs::Function<'js>,
      ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
        Self::on(this, ctx, event, listener)
      }

      /// Playwright/Node: `once(event, listener)`.
      ///
      /// # Errors
      ///
      /// Throws an `Error` when the listener registry is unavailable.
      #[qjs(rename = "once")]
      pub fn once<'js>(
        this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
        ctx: rquickjs::Ctx<'js>,
        event: String,
        listener: rquickjs::Function<'js>,
      ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
        this.0.borrow().register_listener(&ctx, &event, listener, true, false)?;
        Ok(this.0)
      }

      /// Node's `prependListener`.
      ///
      /// # Errors
      ///
      /// Throws an `Error` when the listener registry is unavailable.
      #[qjs(rename = "prependListener")]
      pub fn prepend_listener<'js>(
        this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
        ctx: rquickjs::Ctx<'js>,
        event: String,
        listener: rquickjs::Function<'js>,
      ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
        this.0.borrow().register_listener(&ctx, &event, listener, false, true)?;
        Ok(this.0)
      }

      /// Node's `prependOnceListener`.
      ///
      /// # Errors
      ///
      /// Throws an `Error` when the listener registry is unavailable.
      #[qjs(rename = "prependOnceListener")]
      pub fn prepend_once_listener<'js>(
        this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
        ctx: rquickjs::Ctx<'js>,
        event: String,
        listener: rquickjs::Function<'js>,
      ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
        this.0.borrow().register_listener(&ctx, &event, listener, true, true)?;
        Ok(this.0)
      }

      /// Playwright/Node: `off(event, listener?)`.
      ///
      /// # Errors
      ///
      /// Throws when the listener registry is unavailable.
      #[qjs(rename = "off")]
      pub fn off<'js>(
        this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
        ctx: rquickjs::Ctx<'js>,
        event: String,
        listener: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
        this.0.borrow().off_impl(&ctx, &event, listener.0.as_ref())?;
        Ok(this.0)
      }

      /// Node's `removeListener`, an alias of [`Self::off`].
      ///
      /// # Errors
      ///
      /// Throws when the listener registry is unavailable.
      #[qjs(rename = "removeListener")]
      pub fn remove_listener<'js>(
        this: rquickjs::function::This<rquickjs::class::Class<'js, Self>>,
        ctx: rquickjs::Ctx<'js>,
        event: String,
        listener: rquickjs::function::Opt<rquickjs::Value<'js>>,
      ) -> rquickjs::Result<rquickjs::class::Class<'js, Self>> {
        Self::off(this, ctx, event, listener)
      }

      /// Node's `rawListeners(type)`.
      ///
      /// # Errors
      ///
      /// Throws when the listener registry is unavailable or a callback cannot be restored.
      #[qjs(rename = "rawListeners")]
      pub fn raw_listeners<'js>(
        &self,
        ctx: rquickjs::Ctx<'js>,
        event: String,
      ) -> rquickjs::Result<Vec<rquickjs::Value<'js>>> {
        self.listeners(ctx, event)
      }

      /// Node's `listenerCount(type)`.
      #[qjs(rename = "listenerCount")]
      #[must_use]
      pub fn listener_count(&self, event: String) -> f64 {
        $crate::bindings::convert::usize_to_f64(self.inner.listener_count(&event))
      }

      /// Node's `eventNames()`.
      #[qjs(rename = "eventNames")]
      #[must_use]
      pub fn event_names(&self) -> Vec<String> {
        self.inner.event_names()
      }

      /// Node's `setMaxListeners(n)`.
      #[qjs(rename = "setMaxListeners")]
      #[must_use]
      pub fn set_max_listeners(
        this: rquickjs::function::This<rquickjs::class::Class<'_, Self>>,
        max: f64,
      ) -> rquickjs::class::Class<'_, Self> {
        this
          .0
          .borrow()
          .inner
          .set_max_listeners($crate::bindings::convert::f64_to_usize(max));
        this.0
      }

      /// Node's `getMaxListeners()`.
      #[qjs(rename = "getMaxListeners")]
      #[must_use]
      pub fn get_max_listeners(&self) -> f64 {
        $crate::bindings::convert::usize_to_f64(self.inner.max_listeners())
      }

      $($items)*
    ]);
  };
}

pub(crate) use qjs_impl;
