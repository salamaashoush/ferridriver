//! `CdpPage` JavaScript evaluation, frame queries, element resolution, and content.

use super::*;

impl CdpPage {
  // ---- JavaScript ----

  /// The utility world `element` belongs to, when that is known without
  /// asking the browser: the element is this page's own (not a child
  /// renderer's) and the page has one frame, whose utility world has been
  /// announced.
  #[must_use]
  pub fn known_utility_context(&self, element: &CdpElement) -> Option<i64> {
    if element.session_id().as_deref() != self.session_id.as_deref() {
      return None;
    }
    let frames = self
      .frame_contexts
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut ids = frames.keys();
    let (Some(only), None) = (ids.next(), ids.next()) else {
      return None;
    };
    self
      .utility_contexts
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(only)
      .copied()
  }

  /// # Errors
  ///
  /// Returns an error if the injected script cannot be installed in the page.
  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  /// Ensures the selector engine is injected into the current execution context.
  /// Idempotent and navigation-aware.
  ///
  /// # Errors
  ///
  /// Returns an error if the injected script cannot be installed in the page.
  pub async fn ensure_engine_injected(&self) -> Result<()> {
    self.injected_script.ensure(self).await
  }

  /// Idempotently fire `Page.setInterceptFileChooserDialog({enabled:true})`.
  /// Mirrors Playwright's `_updateFileChooserInterception`
  /// (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:1009`):
  /// the CDP command is a no-op until any test path needs file
  /// chooser events, at which point this method is called and the
  /// command fires once. Subsequent calls are no-ops.
  ///
  /// # Errors
  ///
  /// Never fails: a rejected `Page.setInterceptFileChooserDialog` is ignored, and the `Result` keeps the
  /// signature every backend shares.
  pub async fn enable_file_chooser_intercept(&self) -> Result<()> {
    if self
      .file_chooser_intercept_enabled
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      return Ok(());
    }
    let _ = self
      .cmd(
        "Page.setInterceptFileChooserDialog",
        serde_json::json!({ "enabled": true }),
      )
      .await;
    Ok(())
  }

  /// Idempotently fire `Browser.setDownloadBehavior` so download
  /// events flow. Mirrors Playwright's per-context download config
  /// (`/tmp/playwright/packages/playwright-core/src/server/chromium/crBrowser.ts:354`)
  /// but lazy: not fired until a test path actually needs downloads.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Browser.setDownloadBehavior`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn enable_download_behavior(&self) -> Result<()> {
    if self
      .download_behavior_enabled
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      return Ok(());
    }
    // `allowAndName` writes each download to `<downloadPath>/<guid>`
    // rather than the server's suggested name, so parallel downloads
    // sharing a tempdir cannot collide. `deny` still needs
    // `eventsEnabled` and a path: Chrome reports the refusal as a
    // `downloadProgress` state of `canceled`, which is how
    // `download.path()` learns to reject.
    let behavior = if self.accept_downloads.load(std::sync::atomic::Ordering::Relaxed) {
      "allowAndName"
    } else {
      "deny"
    };
    let mut params = serde_json::json!({
      "behavior": behavior,
      "downloadPath": self.downloads_dir.browser_path(),
      "eventsEnabled": true,
    });
    if let Some(ref ctx) = self.browser_context_id
      && let Some(map) = params.as_object_mut()
    {
      map.insert(
        "browserContextId".into(),
        serde_json::Value::String((**ctx).to_string()),
      );
    }
    self.cmd("Browser.setDownloadBehavior", params).await.map(|_| ())
  }

  /// Evaluate `expression` directly inside the frame's isolated `UTILITY_WORLD`
  /// execution context (where DOM prototypes and element setters are untouched
  /// by page/framework scripts). Reuses the cached `utility_contexts` entry
  /// populated by `ContextSnapshots` + `context_observer`, lazily creating the
  /// world once via `Page.createIsolatedWorld` only when absent or stale.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.createIsolatedWorld` or
  /// `Runtime.evaluate`, the target has closed, or the expression throws.
  pub async fn evaluate_in_utility_world(
    &self,
    frame_id: Option<&str>,
    expression: &str,
  ) -> Result<Option<serde_json::Value>> {
    if let Some(fid) = frame_id
      && let Some(renderer) = self.page_for_frame(fid).await?
    {
      return Box::pin(renderer.evaluate_in_utility_world(Some(fid), expression)).await;
    }
    let fid = match frame_id {
      Some(id) => id.to_owned(),
      None => self.peek_main_frame_id().unwrap_or_else(|| self.target_id.to_string()),
    };
    let mut ctx_id = self
      .utility_contexts
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(&fid)
      .copied();
    for attempt in 0..2 {
      let context_id = if let Some(id) = ctx_id {
        id
      } else {
        let world = self
          .cmd(
            "Page.createIsolatedWorld",
            serde_json::json!({
              "frameId": fid,
              "worldName": UTILITY_WORLD,
              "grantUniveralAccess": true,
            }),
          )
          .await?;
        let id = world["executionContextId"]
          .as_i64()
          .ok_or_else(|| FerriError::protocol("Page.createIsolatedWorld", "missing execution context"))?;
        self
          .utility_contexts
          .write()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .insert(fid.clone(), id);
        id
      };
      let eval_res = self
        .cmd(
          "Runtime.evaluate",
          serde_json::json!({
            "expression": expression,
            "contextId": context_id,
            "returnByValue": true,
            "awaitPromise": true,
          }),
        )
        .await;
      match eval_res {
        Ok(result) => {
          if let Some(exception) = result.get("exceptionDetails") {
            let text = exception
              .get("text")
              .and_then(|v| v.as_str())
              .unwrap_or("Evaluation error");
            return Err(FerriError::Backend(text.to_string()));
          }
          return Ok(result.get("result").and_then(|r| r.get("value")).cloned());
        },
        Err(err) if attempt == 0 && err.to_string().contains("Cannot find context") => {
          self
            .utility_contexts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&fid);
          ctx_id = None;
        },
        Err(err) => return Err(err),
      }
    }
    Err(FerriError::backend("failed to evaluate in utility world"))
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.evaluate`, the target has closed, or the command outlives
  /// its time budget. Also returns [`FerriError::Backend`] carrying the exception text when the expression
  /// throws.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true,
        }),
      )
      .await?;

    if let Some(exception) = result.get("exceptionDetails") {
      let text = exception
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("Evaluation error");
      return Err(FerriError::Backend(text.to_string()));
    }

    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  pub(super) fn peek_frame_context(&self, frame_id: &str) -> Option<i64> {
    self
      .frame_contexts
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(frame_id)
      .copied()
  }

  pub(super) async fn resolve_frame_context(&self, frame_id: &str) -> Result<i64> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
      // Register the waiter BEFORE checking the map (pin + enable, same
      // idiom as `settle_navigation`) so an insert landing between the
      // check and the await still wakes us.
      let notified = self.frame_contexts_notify.notified();
      tokio::pin!(notified);
      notified.as_mut().enable();
      if let Some(id) = self.peek_frame_context(frame_id) {
        return Ok(id);
      }
      if tokio::time::timeout_at(deadline, notified).await.is_err() {
        return Err(FerriError::Backend(format!(
          "No execution context found for frame '{frame_id}'. Frame may not be loaded yet."
        )));
      }
    }
  }

  /// ferridriver's equivalent of Playwright's
  /// `evaluateExpression(context, expr, { returnByValue, isFunction }, ...args)`
  /// (`/tmp/playwright/packages/playwright-core/src/server/javascript.ts:248`).
  /// `args` are the variadic positional arguments passed to the user
  /// function after isomorphic serialization — for `page.evaluate(fn, arg)`
  /// that's `[arg]`; for `handle.evaluate(fn, arg)` it's `[handle, arg]`
  /// with the handle supplied via `{h: 0}` in `args[0]` and its wire
  /// ref in `handles[0]`. There is no separate receiver/`this`
  /// binding — Playwright doesn't have one either.
  ///
  /// # Errors
  ///
  /// Returns a String error on protocol failure, `exceptionDetails`
  /// from the page, or backend/handle mismatch.
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    if let Some(frame) = frame_id
      && let Some(renderer) = self.page_for_frame(frame).await?
    {
      return Box::pin(renderer.call_utility_evaluate(
        fn_source,
        args,
        handles,
        frame_id,
        is_function,
        return_by_value,
      ))
      .await;
    }
    if frame_id.is_none()
      && let Some(crate::protocol::HandleId::Cdp { session_id, .. }) = handles.first()
      && session_id.as_deref() != self.session_id.as_deref()
    {
      let renderer = self.page_for_session(session_id.as_deref())?;
      return Box::pin(renderer.call_utility_evaluate(fn_source, args, handles, None, is_function, return_by_value))
        .await;
    }
    self
      .call_utility_in_renderer(fn_source, args, handles, frame_id, is_function, return_by_value)
      .await
  }

  async fn call_utility_in_renderer(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    self.ensure_engine_injected().await?;

    let context_id = match frame_id {
      Some(fid) => Some(self.resolve_frame_context(fid).await?),
      None => None,
    };

    Box::pin(self.call_utility_in_context(fn_source, args, handles, context_id, is_function, return_by_value)).await
  }

  /// `WebMCP.invokeTool` on the session that owns `frame_id`, answered by
  /// its `toolResponded` event. `None` when the browser has no `WebMCP`
  /// domain (Chromium without `DevToolsWebMCPSupport`). Dropping the future
  /// before the answer cancels the invocation in the page.
  pub(crate) async fn webmcp_invoke(
    &self,
    frame_id: &str,
    name: &str,
    input: &serde_json::Value,
  ) -> Result<Option<serde_json::Value>> {
    if let Some(renderer) = self.page_for_frame(frame_id).await? {
      return Box::pin(renderer.webmcp_invoke(frame_id, name, input)).await;
    }
    if let Err(error) = self.cmd("WebMCP.enable", serde_json::json!({})).await {
      if error.to_string().contains("wasn't found") {
        return Ok(None);
      }
      return Err(error);
    }
    // Chromium answers `invokeTool` before the tool runs, so the response
    // event can only be matched once the id is known; tap before invoking.
    let mut responses = self
      .transport
      .tap_event_methods(&["WebMCP.toolResponded"], self.session_id.as_deref());
    let invoked = self
      .cmd(
        "WebMCP.invokeTool",
        serde_json::json!({"frameId": frame_id, "toolName": name, "input": input}),
      )
      .await?;
    let id = invoked["invocationId"]
      .as_str()
      .ok_or_else(|| FerriError::protocol("WebMCP.invokeTool", "missing invocationId"))?
      .to_owned();
    let mut pending = PendingWebMcpInvocation {
      transport: Arc::clone(&self.transport),
      session_id: self.session_id.clone(),
      id: Some(id.clone()),
    };
    while let Some(event) = responses.recv().await {
      if event.get("sessionId").and_then(serde_json::Value::as_str) != self.session_id.as_deref() {
        continue;
      }
      let params = &event["params"];
      if params["invocationId"].as_str() == Some(id.as_str()) {
        pending.id = None;
        return Ok(Some(params.clone()));
      }
    }
    Err(FerriError::target_closed(Some(
      "the browser connection closed while a WebMCP tool was running".into(),
    )))
  }

  pub(crate) async fn evaluate_isolated(
    &self,
    source: &str,
    arg: &crate::protocol::SerializedArgument,
    frame_id: &str,
  ) -> Result<crate::protocol::SerializedValue> {
    if let Some(renderer) = self.page_for_frame(frame_id).await? {
      return Box::pin(renderer.evaluate_isolated(source, arg, frame_id)).await;
    }
    let world = self
      .cmd(
        "Page.createIsolatedWorld",
        serde_json::json!({
          "frameId":frame_id, "worldName":"__ferridriver_webmcp",
        }),
      )
      .await?;
    let context = world["executionContextId"]
      .as_i64()
      .ok_or_else(|| FerriError::protocol("Page.createIsolatedWorld", "missing execution context"))?;
    let injected = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
          "expression":crate::selectors::UTILITY_SCRIPT_JS, "contextId":context,
          "returnByValue":true, "awaitPromise":true,
        }),
      )
      .await?;
    if let Some(exception) = injected.get("exceptionDetails") {
      return Err(FerriError::evaluation(cdp_get_exception_message(exception)));
    }
    match self
      .call_utility_in_context(
        source,
        std::slice::from_ref(&arg.value),
        &arg.handles,
        Some(context),
        Some(true),
        true,
      )
      .await?
    {
      crate::js_handle::EvaluateResult::Value(value) => Ok(value),
      crate::js_handle::EvaluateResult::Handle(..) => {
        Err(FerriError::protocol("isolated evaluation", "expected a value"))
      },
    }
  }

  pub(super) async fn call_utility_in_context(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    context_id: Option<i64>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    let args_json = serde_json::to_string(args)?;
    let is_fn_json: serde_json::Value = match is_function {
      Some(true) => serde_json::Value::Bool(true),
      Some(false) => serde_json::Value::Bool(false),
      None => serde_json::Value::Null,
    };

    let count = args.len();

    let mut arguments: Vec<serde_json::Value> = vec![
      serde_json::json!({"value": is_fn_json}),
      serde_json::json!({"value": return_by_value}),
      serde_json::json!({"value": fn_source}),
      serde_json::json!({"value": count}),
      serde_json::json!({"value": args_json}),
    ];
    for handle in handles {
      match handle {
        crate::protocol::HandleId::Cdp {
          object_id: obj_id,
          session_id,
        } => {
          if session_id.as_deref() != self.session_id.as_deref() {
            return Err(FerriError::invalid_argument(
              "handles",
              "handles belong to different renderer sessions",
            ));
          }
          arguments.push(serde_json::json!({"objectId": obj_id}));
        },
        _ => {
          return Err(FerriError::invalid_argument(
            "handles",
            "call_utility_evaluate: non-CDP handle in arg.handles on CDP backend",
          ));
        },
      }
    }

    // Anchor strategy:
    //  1. `executionContextId` (frame_contexts cache hit) — best,
    //     unambiguous, no extra RTT.
    //  2. First handle's `objectId` — Chrome runs the function with
    //     `this` bound to that handle, IN that handle's context. The
    //     wrapper ignores `this`, so this just gives us a free
    //     context anchor without an extra RTT.
    //  3. `Runtime.evaluate` IIFE fallback — avoids `callFunctionOn`
    //     entirely (which needs an anchor) by inlining literal args
    //     into the wrapper expression. Chrome picks the default
    //     execution context for an `evaluate` call without
    //     `contextId`, so no `globalThis` lookup needed.
    //
    //  Prior behaviour fired `Runtime.evaluate("globalThis")` to
    //  obtain an anchoring objectId — costs 1 extra RTT per
    //  evaluate. Path 3 below replaces that with a single
    //  `Runtime.evaluate` (the user's wrapper invocation), saving
    //  one RTT per evaluate that has no handles AND no cached
    //  contextId — i.e. the bench's `page.evaluate(string)` shape.
    if let Some(ctx_id) = context_id {
      let params = serde_json::json!({
        "functionDeclaration": UTILITY_EVAL_WRAPPER,
        "arguments": arguments,
        "returnByValue": return_by_value,
        "awaitPromise": true,
        "userGesture": true,
        "executionContextId": ctx_id,
      });
      let response = self.cmd("Runtime.callFunctionOn", params).await?;
      return self.parse_eval_response(&response, return_by_value);
    }

    if !handles.is_empty() {
      // Anchor on the first handle's objectId — gives Chrome the
      // execution context for free, no extra RTT.
      let anchor = match &handles[0] {
        crate::protocol::HandleId::Cdp { object_id, .. } => object_id.clone(),
        _ => {
          return Err(FerriError::invalid_argument(
            "handles",
            "call_utility_evaluate: non-CDP handle in arg.handles on CDP backend",
          ));
        },
      };
      let params = serde_json::json!({
        "functionDeclaration": UTILITY_EVAL_WRAPPER,
        "arguments": arguments,
        "returnByValue": return_by_value,
        "awaitPromise": true,
        "userGesture": true,
        "objectId": anchor,
      });
      let response = self.cmd("Runtime.callFunctionOn", params).await?;
      return self.parse_eval_response(&response, return_by_value);
    }

    // Every path passes `userGesture` as Playwright's `evaluateWithArguments`
    // does: an evaluated `window.open` is a popup, not a blocked one.
    // No contextId, no handles — use Runtime.evaluate IIFE. Chrome
    // picks the default execution context. Wrap as
    // `(WRAPPER)(literal_args...)`. Args are JS-literal-safe via
    // `serde_json::to_string` (a valid JSON string is a valid JS
    // string literal too).
    let is_fn_lit = match is_function {
      Some(true) => "true",
      Some(false) => "false",
      None => "null",
    };
    let return_by_value_lit = if return_by_value { "true" } else { "false" };
    let fn_source_lit = serde_json::to_string(fn_source)?;
    let args_json_lit = serde_json::to_string(&args_json)?;
    let expression =
      format!("({UTILITY_EVAL_WRAPPER})({is_fn_lit},{return_by_value_lit},{fn_source_lit},{count},{args_json_lit})");
    let response = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
          "expression": expression,
          "returnByValue": return_by_value,
          "awaitPromise": true,
          "userGesture": true,
        }),
      )
      .await?;
    self.parse_eval_response(&response, return_by_value)
  }

  /// Decode a `Runtime.evaluate` / `Runtime.callFunctionOn` response
  /// produced by the `UtilityScript` wrapper (`UTILITY_EVAL_WRAPPER`
  /// callFunctionOn path).
  /// Both shapes return the same `{ result: { value, objectId, type,
  /// subtype, ... } }` envelope, so the decoder is shared.
  fn parse_eval_response(
    &self,
    response: &serde_json::Value,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    if let Some(exception) = response.get("exceptionDetails") {
      return Err(FerriError::Backend(cdp_get_exception_message(exception)));
    }

    let result_obj = response
      .get("result")
      .ok_or_else(|| FerriError::protocol("Runtime.callFunctionOn", "call_utility_evaluate: no result"))?;

    if return_by_value {
      // The wrapper JSON.stringified the isomorphic wire shape so
      // CDP just ships a string back. `result.value` is that string;
      // JSON.parse it and hand to the SerializedValue deserializer.
      // `null` (from `undefined` sentinel) maps to
      // `SerializedValue::Special(Undefined)`.
      let wire = result_obj.get("value").cloned().unwrap_or(serde_json::Value::Null);
      let parsed: crate::protocol::SerializedValue = match wire {
        serde_json::Value::Null => crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined),
        serde_json::Value::String(ref s) => {
          let inner: serde_json::Value = serde_json::from_str(s)
            .map_err(|e| FerriError::Backend(format!("call_utility_evaluate: parse inner JSON: {e}")))?;
          serde_json::from_value(inner)
            .map_err(|e| FerriError::Backend(format!("call_utility_evaluate: parse result: {e}")))?
        },
        other => serde_json::from_value(other)
          .map_err(|e| FerriError::Backend(format!("call_utility_evaluate: parse result: {e}")))?,
      };
      Ok(crate::js_handle::EvaluateResult::Value(parsed))
    } else if let Some(obj_id) = result_obj.get("objectId").and_then(|v| v.as_str()) {
      let is_node = result_obj.get("subtype").and_then(|v| v.as_str()) == Some("node");
      Ok(crate::js_handle::EvaluateResult::Handle(
        crate::js_handle::JSHandleBacking::Remote(crate::js_handle::HandleRemote::Cdp {
          object_id: Arc::from(obj_id),
          session_id: self.session_id.clone(),
        }),
        is_node,
      ))
    } else {
      // No objectId — the CDP result is a primitive (number, string,
      // boolean, null, undefined). Playwright's `createHandle`
      // (`/tmp/playwright/packages/playwright-core/src/server/chromium/crProtocolHelper.ts`)
      // produces a value-backed JSHandle here. Parse the inline
      // `value` into our wire shape and ride it back through
      // `JSHandleBacking::Value`.
      let value = result_obj.get("value").cloned().unwrap_or(serde_json::Value::Null);
      let mut ctx = crate::protocol::SerializationContext::default();
      let serialized = if value.is_null() {
        // CDP encodes `null` literally but encodes `undefined` as a
        // missing `value` field with `type: "undefined"`. Distinguish
        // the two via `type` so jsonValue round-trips faithfully.
        let ty = result_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "undefined" {
          crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
        } else {
          crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null)
        }
      } else {
        crate::protocol::SerializedValue::from_json(&value, &mut ctx)
      };
      Ok(crate::js_handle::EvaluateResult::Handle(
        crate::js_handle::JSHandleBacking::Value(serialized),
        false,
      ))
    }
  }

  // ---- Frames ----

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.getFrameTree`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn get_frame_tree(&self) -> Result<Vec<super::super::FrameInfo>> {
    // Consume the seeded tree captured during the per-page parallel
    // `enable_domains` batch. Skips the per-call `Page.getFrameTree`
    // RTT for the first read after `new_page` — every subsequent read
    // (e.g. after a navigation or iframe attach) goes back to the
    // network. Mirrors Playwright's `frameManager._frameTree` which
    // is seeded from the `Page.getFrameTree` it fires inside
    // `FrameSession._initialize` and then maintained via events
    // (`/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:495-549`).
    {
      let mut guard = match self.seeded_frame_tree.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      if let Some(tree) = guard.take() {
        return Ok(tree);
      }
    }
    let result = self.cmd("Page.getFrameTree", super::super::empty_params()).await?;

    let mut frames = Vec::new();
    if let Some(tree) = result.get("frameTree") {
      collect_frames(tree, &mut frames);
    }

    // Chrome's `Page.getFrameTree` returns the iframe's HTML `name`
    // attribute on the `frame.name` field, but srcdoc iframes can
    // report an empty `name` until the child document settles — the
    // attribute is populated on the parent's element but not yet on
    // the CDP-side frame metadata. Fill in the missing names by
    // evaluating `window.name` in each empty-named child frame.
    let child_indices: Vec<usize> = frames
      .iter()
      .enumerate()
      .filter(|(_, f)| f.parent_frame_id.is_some() && f.name.is_empty())
      .map(|(i, _)| i)
      .collect();
    if !child_indices.is_empty() {
      let frame_ids: Vec<String> = child_indices.iter().map(|&i| frames[i].frame_id.clone()).collect();
      let futs: Vec<_> = frame_ids
        .iter()
        .map(|fid| self.evaluate_in_frame("window.name", fid))
        .collect();
      let results = futures::future::join_all(futs).await;
      for (idx, result) in child_indices.into_iter().zip(results) {
        if let Ok(Some(val)) = result
          && let Some(name) = val.as_str()
          && !name.is_empty()
        {
          frames[idx].name = name.to_string();
        }
      }
    }
    Ok(frames)
  }

  /// Deterministic iframe-element -> content-frame id via
  /// `DOM.describeNode` on the element's remote object. Replaces the
  /// fragile name/url cache heuristic for unnamed / `srcdoc` iframes.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `DOM.describeNode`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn content_frame_id(&self, object_id: &str) -> Result<Option<String>> {
    let res = self
      .cmd("DOM.describeNode", serde_json::json!({ "objectId": object_id }))
      .await?;
    Ok(
      res
        .get("node")
        .and_then(|n| n.get("frameId"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string),
    )
  }

  /// Mark a child frame's owner `<iframe>` element with the child's
  /// frame id via the trace snapshot streamer (mirrors
  /// `crPage.ts::getFrameElement`: `DOM.getFrameOwner` →
  /// `DOM.resolveNode` into the parent's main-world context →
  /// `Runtime.callFunctionOn` with the element as `this`).
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `DOM.getFrameOwner`, `DOM.resolveNode` or `Runtime.callFunctionOn`,
  /// the target has closed, or the command outlives its time budget. Also fails when the frame owner has no
  /// backend node or does not resolve to an object.
  pub async fn mark_snapshot_iframe(
    &self,
    child_frame_id: &str,
    parent_frame_id: &str,
    streamer_global: &str,
  ) -> Result<()> {
    let owner = self
      .cmd("DOM.getFrameOwner", serde_json::json!({ "frameId": child_frame_id }))
      .await?;
    let backend_node_id = owner
      .get("backendNodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.getFrameOwner", "no backendNodeId for frame owner"))?;
    // Resolve the owner element in the parent's world when we know it,
    // and otherwise let the protocol pick the node's own frame — the
    // main frame's context id is frequently absent from the map (its
    // creation event predates the tracker), and WAITING for one that
    // will never arrive cost the full resolve timeout on every snapshot
    // of every action on a page with an iframe.
    let mut params = serde_json::json!({ "backendNodeId": backend_node_id });
    if let Some(ctx_id) = self.peek_frame_context(parent_frame_id) {
      params["executionContextId"] = serde_json::json!(ctx_id);
    }
    let resolved = self.cmd("DOM.resolveNode", params).await?;
    let object_id = resolved
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", "frame owner did not resolve to an object"))?
      .to_string();
    let declaration =
      format!("function(frameId) {{ const s = window[{streamer_global:?}]; if (s) s.markIframe(this, frameId); }}");
    let call = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
          "functionDeclaration": declaration,
          "objectId": object_id,
          "arguments": [{ "value": child_frame_id }],
          "returnByValue": true,
        }),
      )
      .await;
    let _ = self
      .cmd("Runtime.releaseObject", serde_json::json!({ "objectId": object_id }))
      .await;
    call.map(|_| ())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.evaluate`, the target has closed, or the command outlives
  /// its time budget. Also fails when the frame has no execution context within 2 seconds, or the expression
  /// throws.
  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<serde_json::Value>> {
    if let Some(renderer) = self.page_for_frame(frame_id).await? {
      return Box::pin(renderer.evaluate_in_frame(expression, frame_id)).await;
    }
    // The main frame's default context often never reaches the map: its
    // `Runtime.executionContextCreated` fires while the page is being
    // set up, before the tracker subscribes, and `setContent` replaces
    // the document without minting a new one. Waiting for it costs the
    // full resolve timeout and then fails — an evaluate in the main
    // frame is a plain evaluate, which is what the default context is.
    if self.peek_main_frame_id().as_deref() == Some(frame_id)
      && !self
        .frame_contexts
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(frame_id)
    {
      return self.evaluate(expression).await;
    }
    let ctx_id = self.resolve_frame_context(frame_id).await?;
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": expression,
            "contextId": ctx_id,
            "returnByValue": true,
            "awaitPromise": true,
        }),
      )
      .await?;

    if let Some(exception) = result.get("exceptionDetails") {
      let text = exception
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("Evaluation error");
      return Err(FerriError::Backend(text.to_string()));
    }
    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  // ---- Elements ----

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `DOM.getDocument` or `DOM.querySelector`, the target has closed, or
  /// the command outlives its time budget. Returns [`FerriError::InvalidSelector`] when nothing matches
  /// `selector`.
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    let doc = self.cmd("DOM.getDocument", serde_json::json!({"depth": 0})).await?;
    let root_id = doc
      .get("root")
      .and_then(|r| r.get("nodeId"))
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.getDocument", "No document root"))?;

    let result = self
      .cmd(
        "DOM.querySelector",
        serde_json::json!({"nodeId": root_id, "selector": selector}),
      )
      .await?;

    let node_id = result
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.querySelector", format!("'{selector}' not found")))?;

    if node_id == 0 {
      return Err(FerriError::invalid_selector(selector, "not found"));
    }

    Ok(AnyElement::Cdp(CdpElement {
      transport: Arc::clone(&self.transport),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: Some(node_id),
        object_id: None,
      })),
    }))
  }

  /// Construct a [`CdpElement`] directly from a `Runtime.RemoteObjectId`
  /// without re-resolving through the DOM. Used by
  /// [`crate::backend::element_from_remote`] when a [`crate::js_handle::JSHandle`]
  /// turns out to wrap a DOM node and needs to be re-packaged as an
  /// [`crate::element_handle::ElementHandle`] — the remote is already
  /// addressable, so we can skip a round-trip by seeding the element's
  /// `object_id` slot directly.
  pub(crate) fn element_from_object_id(&self, object_id: Arc<str>, session_id: Option<Arc<str>>) -> CdpElement {
    CdpElement {
      transport: Arc::clone(&self.transport),
      session_id,
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: None,
        object_id: Some(object_id),
      })),
    }
  }

  /// Evaluate `js` in the execution context of `frame_id` (or the main
  /// page when `frame_id` is `None`) and return the resulting DOM
  /// element. Used by `Locator` to scope action-method resolution to
  /// the locator's bound `Frame` — Playwright parity.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.evaluate`, the target has closed, or the command outlives
  /// its time budget. Also fails when the frame has no execution context within 2 seconds, the script throws,
  /// or it does not return a DOM element.
  pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement> {
    if let Some(frame) = frame_id
      && let Some(renderer) = self.page_for_frame(frame).await?
    {
      return Box::pin(renderer.evaluate_to_element(js, frame_id)).await;
    }
    // Note: previously fired `DOM.getDocument` here but discarded the
    // result. It was a leftover from when this path used
    // `DOM.querySelector` (which DOES require the agent's DOM tree
    // to be populated). The current `Runtime.evaluate` path returns
    // a `RemoteObjectId` directly from the renderer's V8 context,
    // independent of the DOM agent state — no priming needed.
    // Saves 1 RTT per locator action / element handle resolve.

    // Resolve the frame's execution context id (None → main page).
    let context_id = match frame_id {
      Some(fid) => Some(self.resolve_frame_context(fid).await?),
      None => None,
    };

    let mut params = serde_json::json!({
      "expression": js,
      "returnByValue": false,
    });
    if let Some(ctx_id) = context_id {
      params["contextId"] = serde_json::json!(ctx_id);
    }

    let result = self.cmd("Runtime.evaluate", params).await?;

    if let Some(exception) = result.get("exceptionDetails") {
      // `text` is typically the short prefix Chrome attaches
      // ("Uncaught"); the real message lives on
      // `exception.exception.description` (V8 format) or
      // `exception.exception.value`. Combine so the caller's
      // `err.contains("strict mode violation")` / similar checks
      // actually see the engine-thrown payload.
      let text = exception.get("text").and_then(|v| v.as_str()).unwrap_or("");
      let inner = exception
        .get("exception")
        .and_then(|e| {
          e.get("description")
            .and_then(|v| v.as_str())
            .or_else(|| e.get("value").and_then(|v| v.as_str()))
        })
        .unwrap_or("");
      let combined = match (text.is_empty(), inner.is_empty()) {
        (false, false) => format!("{text}: {inner}"),
        (false, true) => text.to_string(),
        (true, false) => inner.to_string(),
        (true, true) => "Evaluation error".to_string(),
      };
      return Err(FerriError::evaluation(combined));
    }

    let object_id = result
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "JS did not return a DOM element"))?;

    Ok(AnyElement::Cdp(CdpElement {
      transport: Arc::clone(&self.transport),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: None,
        object_id: Some(Arc::from(object_id)),
      })),
    }))
  }

  // ---- Content ----

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.evaluate`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn content(&self) -> Result<String> {
    let result = self
      .cmd(
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "document.documentElement.outerHTML",
            "returnByValue": true,
        }),
      )
      .await?;
    Ok(
      result
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string(),
    )
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Page.getFrameTree` or `Page.setDocumentContent`, the target has
  /// closed, or the command outlives its time budget. Also fails when the frame tree has no main frame.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    let frame_id = self
      .main_frame_id
      .get_or_try_init(|| async {
        let tree = self.cmd("Page.getFrameTree", super::super::empty_params()).await?;
        tree
          .get("frameTree")
          .and_then(|f| f.get("frame"))
          .and_then(|f| f.get("id"))
          .and_then(|v| v.as_str())
          .map(std::string::ToString::to_string)
          .ok_or_else(|| FerriError::protocol("Page.getFrameTree", "no main frame"))
      })
      .await?;

    // Selector engine is already injected via Page.addScriptToEvaluateOnNewDocument
    // during page setup, so setDocumentContent triggers it automatically.
    self
      .cmd(
        "Page.setDocumentContent",
        serde_json::json!({"frameId": frame_id, "html": html}),
      )
      .await?;
    Ok(())
  }

  /// Set the main frame's HTML and immediately evaluate `check_js` in a single
  /// pipelined CDP batch (strict FIFO ordering on the page session), saving a
  /// sequential round trip on every `page.setContent()`.
  ///
  /// # Errors
  ///
  /// Returns an error if resolving the main frame id or `Page.setDocumentContent`
  /// fails; an evaluation error in `check_js` is surfaced as `Ok(None)` so the
  /// caller falls back to lifecycle waiting.
  pub async fn set_content_and_check(&self, html: &str, check_js: &str) -> Result<Option<serde_json::Value>> {
    let frame_id = self
      .main_frame_id
      .get_or_try_init(|| async {
        let tree = self.cmd("Page.getFrameTree", super::super::empty_params()).await?;
        tree
          .get("frameTree")
          .and_then(|f| f.get("frame"))
          .and_then(|f| f.get("id"))
          .and_then(|v| v.as_str())
          .map(std::string::ToString::to_string)
          .ok_or_else(|| FerriError::protocol("Page.getFrameTree", "no main frame"))
      })
      .await?;

    let set_fut = self.cmd(
      "Page.setDocumentContent",
      serde_json::json!({"frameId": frame_id, "html": html}),
    );
    let check_fut = self.cmd(
      "Runtime.evaluate",
      serde_json::json!({
        "expression": check_js,
        "returnByValue": true,
        "awaitPromise": false,
      }),
    );
    let (set_res, check_res) = tokio::join!(set_fut, check_fut);
    set_res?;
    Ok(check_res.ok().and_then(|res| {
      if res.get("exceptionDetails").is_some() {
        return None;
      }
      res.get("result").and_then(|r| r.get("value")).cloned()
    }))
  }
}
