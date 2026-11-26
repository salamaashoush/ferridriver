//! Frame-scoped JavaScript evaluation, `waitForFunction`, and script/style tag injection.

use std::sync::Arc;

use crate::error::Result;

use super::Frame;
use super::navigation::deadline_after;

impl Frame {
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
    self
      .page
      .traced_as(
        "Frame",
        "evaluate",
        serde_json::json!({ "expression": fn_source }),
        self.evaluate_untraced(fn_source, arg, is_function),
      )
      .await
  }

  /// [`Self::evaluate`] without a trace action of its own — what a
  /// public method that already opened one calls.
  pub(crate) async fn evaluate_untraced(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    self.page.expose_arg_functions(&arg.functions).await?;
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
    self
      .page
      .traced_as(
        "Frame",
        "evaluateHandle",
        serde_json::json!({ "expression": fn_source }),
        self.evaluate_handle_untraced(fn_source, arg, is_function),
      )
      .await
  }

  /// [`Self::evaluate_handle`] without a trace action of its own — the
  /// selector probes `$eval` / `$$eval` run are part of those calls.
  pub(crate) async fn evaluate_handle_untraced(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::js_handle::JSHandle> {
    self.page.expose_arg_functions(&arg.functions).await?;
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

  /// Playwright: `frame.waitForFunction(pageFunction, arg?, options?): Promise<JSHandle>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:481`).
  ///
  /// Polls `pageFunction` (with `arg`) in this frame until it returns a
  /// truthy value, then returns that value as a
  /// [`crate::js_handle::JSHandle`]. `options.polling` is `'raf'`
  /// (default) — one poll per animation frame — or a number of
  /// milliseconds between polls. Probe errors from a navigating frame
  /// (context destroyed) re-poll against the new document, mirroring
  /// Playwright's rerunnable-task re-install; other page-side
  /// exceptions propagate immediately.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::timeout`] when the function never
  /// turns truthy before the deadline; page-side exceptions from the
  /// function itself are forwarded.
  ///
  /// Options chain as setters: `.polling(100)`, `.timeout(5_000)`.
  #[track_caller]
  pub fn wait_for_function(
    &self,
    fn_source: &str,
  ) -> crate::action::Action<'static, crate::options::WaitForFunctionOptions, crate::js_handle::JSHandle> {
    self.wait_for_function_action(fn_source, Ok(crate::protocol::SerializedArgument::default()))
  }

  /// [`Self::wait_for_function`] with a serde-serialized argument passed
  /// to the page function, as [`Self::eval_with`].
  ///
  /// # Errors
  ///
  /// See [`Self::wait_for_function`], plus the serialization error when
  /// `arg` cannot be encoded.
  #[track_caller]
  pub fn wait_for_function_with(
    &self,
    fn_source: &str,
    arg: &(impl serde::Serialize + ?Sized),
  ) -> crate::action::Action<'static, crate::options::WaitForFunctionOptions, crate::js_handle::JSHandle> {
    self.wait_for_function_action(fn_source, crate::protocol::argument_from_serde(arg))
  }

  #[track_caller]
  fn wait_for_function_action(
    &self,
    fn_source: &str,
    arg: Result<crate::protocol::SerializedArgument>,
  ) -> crate::action::Action<'static, crate::options::WaitForFunctionOptions, crate::js_handle::JSHandle> {
    let frame = self.clone();
    let fn_source = fn_source.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .wait_for_function_serialized(&fn_source, arg?, None, Some(opts))
          .await
      })
    })
  }

  /// [`Self::wait_for_function`] for callers holding an already
  /// serialized argument and an explicit function-or-expression flag
  /// (the bindings).
  ///
  /// # Errors
  ///
  /// See [`Self::wait_for_function`].
  pub async fn wait_for_function_serialized(
    &self,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
    options: Option<crate::options::WaitForFunctionOptions>,
  ) -> Result<crate::js_handle::JSHandle> {
    let opts = options.unwrap_or_default();
    let polling = opts.polling.unwrap_or(crate::options::Polling::Raf);
    let timeout = opts.timeout.unwrap_or_else(|| self.page.default_timeout());
    let deadline = deadline_after(timeout);
    // Truthiness is decided page-side (`!!`) so objects, NaN, and
    // empty-string edge cases follow JS semantics exactly instead of a
    // lossy serialized-value reconstruction.
    // `None` auto-detects as Playwright's predicate does: a source that
    // evaluates to a function is called with `arg`, anything else is the
    // value itself (`server/frames.ts::waitForFunctionExpression`).
    let probe = match is_function {
      Some(true) => format!("arg => !!(({fn_source})(arg))"),
      Some(false) => format!("!!({fn_source})"),
      None => format!("arg => {{ const r = ({fn_source}); return !!(typeof r === 'function' ? r(arg) : r); }}"),
    };
    let probe_is_function = is_function != Some(false);
    let expired = || deadline.is_some_and(|d| tokio::time::Instant::now() >= d);
    let timed_out = || crate::error::FerriError::timeout(format!("waiting for function: {fn_source}"), timeout);
    crate::operation_budget::scoped(timeout, || format!("waiting for function: {fn_source}"), async move {
      loop {
        if expired() {
          return Err(timed_out());
        }
        match self.evaluate(&probe, arg.clone(), Some(probe_is_function)).await {
          Ok(value) => {
            if crate::protocol::result_to_serde::<bool>(&value).unwrap_or(false) {
              break;
            }
          },
          Err(e) => {
            if !crate::locator::is_retryable_action_error(&e.to_string()) {
              return Err(e);
            }
          },
        }
        match polling {
          crate::options::Polling::Interval(ms) => {
            tokio::time::sleep(std::time::Duration::from_millis(ms.max(1))).await;
          },
          crate::options::Polling::Raf => {
            // One animation frame between polls; a failing round-trip
            // (frame mid-navigation) degrades to a frame-length sleep.
            if self
              .evaluate(
                "() => new Promise(f => requestAnimationFrame(() => f(1)))",
                crate::protocol::SerializedArgument::default(),
                Some(true),
              )
              .await
              .is_err()
            {
              tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            }
          },
        }
      }
      // Materialize the truthy result as a handle. The function runs once
      // more; waitForFunction predicates are truthy-stable by contract.
      self.evaluate_handle(fn_source, arg, is_function).await
    })
    .await
  }

  /// Playwright: `frame.$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:242`).
  /// Resolves the first element matching `selector` through the selector
  /// engine, then calls `fn(element, arg)`. Throws when no element
  /// matches, matching Playwright's `evalOnSelector`.
  ///
  /// # Errors
  ///
  /// [`crate::error::FerriError::Evaluation`] when the selector matches
  /// nothing or the user function throws; selector-parse / backend error.
  pub async fn eval_on_selector(
    &self,
    selector: &str,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    let parsed = crate::selectors::parse(selector)?;
    let parts_json = crate::selectors::build_parts_json(&parsed);
    self.page.inner.ensure_engine_injected().await?;
    let sel_str = serde_json::to_string(selector)
      .map_err(|e| crate::error::FerriError::Backend(format!("$eval selector escape: {e}")))?;
    let probe = format!(
      "() => {{ const r = window.__fd.selOne({parts_json}); \
        if (!r) throw new Error('failed to find element matching selector ' + {sel_str}); \
        return r; }}"
    );
    let match_handle = self
      .evaluate_handle_untraced(&probe, crate::protocol::SerializedArgument::default(), Some(true))
      .await?;
    let result = match_handle.evaluate(fn_source, arg, is_function).await;
    let _ = match_handle.dispose().await;
    result
  }

  /// Playwright: `frame.$$eval(selector, pageFunction, arg?): Promise<R>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:248`).
  /// Resolves every element matching `selector` as an array, then calls
  /// `fn(elements, arg)`. Unlike `$eval`, an empty match is not an error
  /// -- the function receives an empty array.
  ///
  /// # Errors
  ///
  /// Returns an error only when the user function throws, or on selector
  /// parse / protocol failure.
  pub async fn eval_on_selector_all(
    &self,
    selector: &str,
    fn_source: &str,
    arg: crate::protocol::SerializedArgument,
    is_function: Option<bool>,
  ) -> Result<crate::protocol::SerializedValue> {
    let parsed = crate::selectors::parse(selector)?;
    let parts_json = crate::selectors::build_parts_json(&parsed);
    self.page.inner.ensure_engine_injected().await?;
    let probe = format!("() => window.__fd.selAll({parts_json})");
    let array_handle = self
      .evaluate_handle_untraced(&probe, crate::protocol::SerializedArgument::default(), Some(true))
      .await?;
    let result = array_handle.evaluate(fn_source, arg, is_function).await;
    let _ = array_handle.dispose().await;
    result
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
  pub(super) async fn backend_eval_expr(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    if self.is_main_frame() {
      self.page.inner.evaluate(expression).await
    } else {
      self.page.inner.evaluate_in_frame(expression, &self.id).await
    }
  }

  /// Playwright's `frame.addScriptTag(options?: { content?, path?, type?,
  /// url? }): Promise<ElementHandle>`. Adds a `<script>` to this frame's
  /// `<head>` and returns it once the script has loaded or run.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the script fails to load.
  ///
  /// Options chain as setters: `.content(..)`, `.url(..)`, `.path(..)`.
  #[track_caller]
  pub fn add_script_tag(
    &self,
  ) -> crate::action::Action<'static, crate::options::AddScriptTagOptions, crate::element_handle::ElementHandle> {
    let frame = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.add_script_tag_impl(opts).await }))
  }

  async fn add_script_tag_impl(
    &self,
    options: crate::options::AddScriptTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    self
      .page
      .traced_as(
        "Frame",
        "addScriptTag",
        serde_json::json!({ "url": options.url }),
        self.add_script_tag_untraced(options),
      )
      .await
  }

  async fn add_script_tag_untraced(
    &self,
    options: crate::options::AddScriptTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    const ADD_URL: &str = "async params => {        const script = document.createElement('script');        script.src = params.url;        if (params.type) script.type = params.type;        const loaded = new Promise((resolve, reject) => {          script.onload = resolve;          script.onerror = e => reject(typeof e === 'string' ? new Error(e) : new Error(`Failed to load script at ${script.src}`));        });        document.head.appendChild(script);        await loaded;        return script;      }";
    const ADD_CONTENT: &str = "params => {        const script = document.createElement('script');        script.type = params.type || 'text/javascript';        script.text = params.content;        let error = null;        script.onerror = e => error = e;        document.head.appendChild(script);        if (error) throw error;        return script;      }";
    let script_type = options.script_type.unwrap_or_default();
    let (source, params) = if let Some(url) = options.url {
      (ADD_URL, serde_json::json!({ "url": url, "type": script_type }))
    } else {
      let content = match (options.path, options.content) {
        (Some(path), _) => {
          let content = tokio::fs::read_to_string(&path).await?;
          format!(
            "{content}\n//# sourceURL={}",
            path.display().to_string().replace('\n', "")
          )
        },
        (None, Some(content)) => content,
        (None, None) => return Err(missing_tag_source()),
      };
      (
        ADD_CONTENT,
        serde_json::json!({ "content": content, "type": script_type }),
      )
    };
    self.add_tag(source, &params).await
  }

  /// Playwright: `frame.addStyleTag(options?: { content?, path?, url? }):
  /// Promise<ElementHandle>`. Adds a `<link rel="stylesheet">` for `url`,
  /// or a `<style>` otherwise, to this frame's `<head>` and returns it
  /// once the stylesheet has loaded.
  ///
  /// # Errors
  ///
  /// Returns an error if none of `url`, `path` and `content` is given,
  /// `path` cannot be read, or the stylesheet fails to load.
  ///
  /// Options chain as setters: `.content(..)`, `.url(..)`, `.path(..)`.
  #[track_caller]
  pub fn add_style_tag(
    &self,
  ) -> crate::action::Action<'static, crate::options::AddStyleTagOptions, crate::element_handle::ElementHandle> {
    let frame = self.clone();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.add_style_tag_impl(opts).await }))
  }

  async fn add_style_tag_impl(
    &self,
    options: crate::options::AddStyleTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    self
      .page
      .traced_as(
        "Frame",
        "addStyleTag",
        serde_json::json!({ "url": options.url }),
        self.add_style_tag_untraced(options),
      )
      .await
  }

  async fn add_style_tag_untraced(
    &self,
    options: crate::options::AddStyleTagOptions,
  ) -> Result<crate::element_handle::ElementHandle> {
    const ADD_URL: &str = "async params => {        const link = document.createElement('link');        link.rel = 'stylesheet';        link.href = params.url;        const loaded = new Promise((resolve, reject) => {          link.onload = resolve;          link.onerror = () => reject(new Error(`Failed to load stylesheet at ${link.href}`));        });        document.head.appendChild(link);        await loaded;        return link;      }";
    const ADD_CONTENT: &str = "async params => {        const style = document.createElement('style');        style.type = 'text/css';        style.appendChild(document.createTextNode(params.content));        const loaded = new Promise((resolve, reject) => {          style.onload = resolve;          style.onerror = () => reject(new Error('Failed to load the stylesheet'));        });        document.head.appendChild(style);        await loaded;        return style;      }";
    let (source, params) = if let Some(url) = options.url {
      (ADD_URL, serde_json::json!({ "url": url }))
    } else {
      let content = match (options.path, options.content) {
        (Some(path), _) => {
          let content = tokio::fs::read_to_string(&path).await?;
          format!(
            "{content}/*# sourceURL={}*/",
            path.display().to_string().replace('\n', "")
          )
        },
        (None, Some(content)) => content,
        (None, None) => return Err(missing_tag_source()),
      };
      (ADD_CONTENT, serde_json::json!({ "content": content }))
    };
    self.add_tag(source, &params).await
  }

  /// Run `source`, which adds an element and returns it, and hand that
  /// element back.
  async fn add_tag(&self, source: &str, params: &serde_json::Value) -> Result<crate::element_handle::ElementHandle> {
    let handle = self
      .evaluate_handle_untraced(source, crate::protocol::argument_from_serde(params)?, Some(true))
      .await?;
    handle
      .as_element()
      .ok_or_else(|| crate::error::FerriError::evaluation("the added tag did not come back as an element"))
  }
}

fn missing_tag_source() -> crate::error::FerriError {
  crate::error::FerriError::invalid_argument(
    "options",
    "Provide an object with a `url`, `path` or `content` property",
  )
}
