//! `ElementHandleJs`: `QuickJS` wrapper around `ferridriver::ElementHandle`.
//!
//! Shared element actions, geometry, and screenshot methods come from
//! `qjs_impl!`.

use ferridriver::ElementHandle;
use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::{extract_page_function, quickjs_arg_to_serialized, serialized_value_to_quickjs};

/// Parse Playwright's `options?: { timeout?: number }` bag into a
/// millisecond timeout.
fn parse_timeout_options<'js>(
  ctx: &rquickjs::Ctx<'js>,
  options: rquickjs::function::Opt<rquickjs::Value<'js>>,
) -> rquickjs::Result<Option<u64>> {
  #[derive(serde::Deserialize, Default)]
  #[serde(rename_all = "camelCase", default)]
  struct JsTimeoutOpts {
    timeout: Option<f64>,
  }
  let parsed: JsTimeoutOpts = match options.0 {
    Some(v) if !v.is_undefined() && !v.is_null() => crate::bindings::convert::serde_from_js(ctx, v)?,
    _ => JsTimeoutOpts::default(),
  };
  Ok(parsed.timeout.map(crate::bindings::convert::ms_f64_to_u64))
}

/// QuickJS-visible wrapper around a core [`ElementHandle`].
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "ElementHandle")]
pub struct ElementHandleJs {
  #[qjs(skip_trace)]
  inner: ElementHandle,
}

impl ElementHandleJs {
  #[must_use]
  pub fn new(inner: ElementHandle) -> Self {
    Self { inner }
  }

  #[must_use]
  pub fn inner(&self) -> &ElementHandle {
    &self.inner
  }
}

crate::bindings::macros::qjs_impl! {
  impl ElementHandleJs {
    @mixin element_actions();

    /// Playwright `elementHandle.isDisposed(): boolean` — METHOD (not a property).
    #[qjs(rename = "isDisposed")]
    #[must_use]
    pub fn is_disposed(&self) -> bool {
      self.inner.is_disposed()
    }

    /// Release the underlying remote element. Idempotent.
    ///
    /// # Errors
    ///
    /// Rejects with the core error under its Playwright name when releasing fails.
    #[qjs(rename = "dispose")]
    pub async fn dispose(&self, call_site: crate::bindings::CallSite, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
      call_site
        .scope(async move { self.inner.dispose().await.into_js_with(&ctx) })
        .await
    }

    /// Companion [`crate::bindings::js_handle::JSHandleJs`] sharing the same remote reference.
    #[qjs(rename = "asJSHandle")]
    #[must_use]
    pub fn as_js_handle(&self) -> crate::bindings::js_handle::JSHandleJs {
      crate::bindings::js_handle::JSHandleJs::new(self.inner.as_js_handle().clone())
    }

    /// Playwright: `elementHandle.evaluate(pageFunction, arg?): Promise<R>`.
    ///
    /// # Errors
    ///
    /// Rejects when `pageFunction`, `arg`, or `options` cannot be converted, or on evaluation failure.
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
              Some(self.inner.page().backend_page_id()),
            )?,
          )?;
          let result = self
            .inner
            .as_js_handle()
            .evaluate(&source, serialized, is_fn)
            .await
            .into_js_with(&ctx)?;
          serialized_value_to_quickjs(&ctx, &result)
        })
        .await
    }

    /// Playwright: `elementHandle.evaluateHandle(pageFunction, arg?): Promise<JSHandle>`.
    ///
    /// # Errors
    ///
    /// Rejects when `pageFunction`, `arg`, or `options` cannot be converted, or on evaluation failure.
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
              Some(self.inner.page().backend_page_id()),
            )?,
          )?;
          let handle = self
            .inner
            .as_js_handle()
            .evaluate_handle(&source, serialized, is_fn)
            .await
            .into_js_with(&ctx)?;
          Ok(crate::bindings::js_handle::JSHandleJs::new(handle))
        })
        .await
    }

    // ── Content reads ────────────────────────────────────────────────────

    /// Playwright's `elementHandle.innerHTML`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "innerHTML")]
    pub async fn inner_html(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<String> {
      call_site
        .scope(async move { self.inner.inner_html().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.innerText`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "innerText")]
    pub async fn inner_text(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<String> {
      call_site
        .scope(async move { self.inner.inner_text().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.textContent`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "textContent")]
    pub async fn text_content(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<crate::bindings::convert::Null<String>> {
      call_site
        .scope(async move { self.inner.text_content().await.into_js_with(&ctx) })
        .await
        .map(crate::bindings::convert::Null)
    }

    /// Playwright's `elementHandle.getAttribute`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "getAttribute")]
    pub async fn get_attribute(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
      name: String,
    ) -> rquickjs::Result<crate::bindings::convert::Null<String>> {
      call_site
        .scope(async move { self.inner.get_attribute(&name).await.into_js_with(&ctx) })
        .await
        .map(crate::bindings::convert::Null)
    }

    /// Playwright's `elementHandle.inputValue`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "inputValue")]
    pub async fn input_value(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<String> {
      call_site
        .scope(async move { self.inner.input_value().await.into_js_with(&ctx) })
        .await
    }

    // ── State predicates ─────────────────────────────────────────────────

    /// Playwright's `elementHandle.isVisible`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "isVisible")]
    pub async fn is_visible(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<bool> {
      call_site
        .scope(async move { self.inner.is_visible().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.isHidden`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "isHidden")]
    pub async fn is_hidden(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<bool> {
      call_site
        .scope(async move { self.inner.is_hidden().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.isDisabled`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "isDisabled")]
    pub async fn is_disabled(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<bool> {
      call_site
        .scope(async move { self.inner.is_disabled().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.isEnabled`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "isEnabled")]
    pub async fn is_enabled(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<bool> {
      call_site
        .scope(async move { self.inner.is_enabled().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.isChecked`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "isChecked")]
    pub async fn is_checked(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<bool> {
      call_site
        .scope(async move { self.inner.is_checked().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.isEditable`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "isEditable")]
    pub async fn is_editable(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
    ) -> rquickjs::Result<bool> {
      call_site
        .scope(async move { self.inner.is_editable().await.into_js_with(&ctx) })
        .await
    }

    /// Playwright's `elementHandle.type`.
    ///
    /// # Errors
    ///
    /// Rejects with a `TypeError` for malformed options, or the core error on failure.
    #[qjs(rename = "type")]
    pub async fn type_str<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
      text: String,
      options: rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<()> {
      call_site
        .scope(async move {
          let opts = crate::bindings::convert::parse_type_options(&ctx, options)?;
          self.inner.type_str(&text).maybe_options(opts).await.into_js_with(&ctx)
        })
        .await
    }

    // ── $eval / $$eval ───────────────────────────────────────────────────

    /// Playwright: `elementHandle.$eval(selector, pageFunction, arg?)`.
    ///
    /// # Errors
    ///
    /// Rejects when `pageFunction` or `arg` cannot be converted, or on evaluation failure.
    #[qjs(rename = "$eval")]
    pub async fn dollar_eval<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
      selector: String,
      page_function: rquickjs::Value<'js>,
      arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
      call_site
        .scope(async move {
          let (source, _is_fn) = extract_page_function(&ctx, page_function)?;
          let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
          let result = self
            .inner
            .eval_on_selector(&selector, &source, serialized)
            .await
            .into_js_with(&ctx)?;
          serialized_value_to_quickjs(&ctx, &result)
        })
        .await
    }

    /// Playwright: `elementHandle.$$eval(selector, pageFunction, arg?)`.
    ///
    /// # Errors
    ///
    /// Rejects when `pageFunction` or `arg` cannot be converted, or on evaluation failure.
    #[qjs(rename = "$$eval")]
    pub async fn dollar_dollar_eval<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
      selector: String,
      page_function: rquickjs::Value<'js>,
      arg: rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<rquickjs::Value<'js>> {
      call_site
        .scope(async move {
          let (source, _is_fn) = extract_page_function(&ctx, page_function)?;
          let serialized = quickjs_arg_to_serialized(&ctx, arg.0)?;
          let result = self
            .inner
            .eval_on_selector_all(&selector, &source, serialized)
            .await
            .into_js_with(&ctx)?;
          serialized_value_to_quickjs(&ctx, &result)
        })
        .await
    }

    // ── $ / $$ ───────────────────────────────────────────────────────────

    /// Playwright: `elementHandle.$(selector): Promise<ElementHandle | null>`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "$")]
    pub async fn query_selector(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
      selector: String,
    ) -> rquickjs::Result<crate::bindings::convert::Null<ElementHandleJs>> {
      call_site
        .scope(async move {
          let maybe = self.inner.query_selector(&selector).await.into_js_with(&ctx)?;
          Ok(maybe.map(ElementHandleJs::new))
        })
        .await
        .map(crate::bindings::convert::Null)
    }

    /// Playwright: `elementHandle.$$(selector): Promise<ElementHandle[]>`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "$$")]
    pub async fn query_selector_all(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'_>,
      selector: String,
    ) -> rquickjs::Result<Vec<ElementHandleJs>> {
      call_site
        .scope(async move {
          let handles = self.inner.query_selector_all(&selector).await.into_js_with(&ctx)?;
          Ok(handles.into_iter().map(ElementHandleJs::new).collect())
        })
        .await
    }

    // ── Frame accessors ──────────────────────────────────────────────────

    /// Playwright: `elementHandle.ownerFrame(): Promise<Frame | null>`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "ownerFrame")]
    pub async fn owner_frame<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
    ) -> rquickjs::Result<crate::bindings::convert::Null<rquickjs::Class<'js, crate::bindings::frame::FrameJs>>> {
      call_site
        .scope(async move {
          let maybe = self.inner.owner_frame().await.into_js_with(&ctx)?;
          maybe
            .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
            .transpose()
        })
        .await
        .map(crate::bindings::convert::Null)
    }

    /// Playwright: `elementHandle.contentFrame(): Promise<Frame | null>`.
    ///
    /// # Errors
    ///
    /// Rejects with the core error on failure.
    #[qjs(rename = "contentFrame")]
    pub async fn content_frame<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
    ) -> rquickjs::Result<crate::bindings::convert::Null<rquickjs::Class<'js, crate::bindings::frame::FrameJs>>> {
      call_site
        .scope(async move {
          let maybe = self.inner.content_frame().await.into_js_with(&ctx)?;
          maybe
            .map(|frame| crate::bindings::identity::frame_object(&ctx, frame))
            .transpose()
        })
        .await
        .map(crate::bindings::convert::Null)
    }

    // ── Wait helpers ─────────────────────────────────────────────────────

    /// Playwright: `elementHandle.waitForElementState(state, options?: { timeout? })`.
    ///
    /// # Errors
    ///
    /// Rejects with a `FerriError`, `TypeError`, or `TimeoutError`.
    #[qjs(rename = "waitForElementState")]
    pub async fn wait_for_element_state<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
      state: String,
      options: rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<()> {
      call_site
        .scope(async move {
          let st = ferridriver::ElementState::parse(&state).into_js_with(&ctx)?;
          let timeout_ms = parse_timeout_options(&ctx, options)?;
          self
            .inner
            .wait_for_element_state(st, timeout_ms)
            .await
            .into_js_with(&ctx)
        })
        .await
    }

    /// Playwright: `elementHandle.waitForSelector(selector, options?: { timeout? })`.
    ///
    /// # Errors
    ///
    /// Rejects with a `TypeError` or `TimeoutError`.
    #[qjs(rename = "waitForSelector")]
    pub async fn wait_for_selector<'js>(
      &self,
      call_site: crate::bindings::CallSite,
      ctx: rquickjs::Ctx<'js>,
      selector: String,
      options: rquickjs::function::Opt<rquickjs::Value<'js>>,
    ) -> rquickjs::Result<crate::bindings::convert::Null<ElementHandleJs>> {
      call_site
        .scope(async move {
          let timeout_ms = parse_timeout_options(&ctx, options)?;
          let maybe = self
            .inner
            .wait_for_selector(&selector, timeout_ms)
            .await
            .into_js_with(&ctx)?;
          Ok(maybe.map(ElementHandleJs::new))
        })
        .await
        .map(crate::bindings::convert::Null)
    }
  }
}
