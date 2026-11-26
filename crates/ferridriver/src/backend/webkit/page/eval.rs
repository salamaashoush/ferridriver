//! `WebKitPage` JavaScript evaluation, elements, content, and handle/utility evaluation.

use super::*;

impl WebKitPage {
  // ── JavaScript ────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns the error from [`Self::ensure_engine_injected`] when the selector engine cannot be
  /// injected.
  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  /// See [`crate::backend::AnyPage::forget_engine`].
  pub fn forget_engine(&self) {
    self.engine_injected.reset();
  }

  /// # Errors
  ///
  /// Returns an error when injecting the selector engine through `Runtime.callFunctionOn` fails, or
  /// `FerriError::Evaluation` when the injection script throws. A failure caused by the document
  /// being replaced mid-call is retried up to `ENGINE_ATTEMPTS` times first.
  pub async fn ensure_engine_injected(&self) -> Result<()> {
    // Until an injection lands in the document that is still current: one
    // that finishes after a navigation went into the document it replaced.
    let mut gone = 0;
    while let Some(document) = self.engine_injected.pending() {
      match Box::pin(self.inject_engine()).await {
        Ok(()) if self.engine_injected.injected(document) => break,
        Ok(()) => {},
        Err(error) if gone < crate::backend::cdp::ENGINE_ATTEMPTS && error.to_string().contains(STALE_ANCHOR) => {
          gone += 1;
        },
        Err(error) => return Err(error),
      }
    }
    Ok(())
  }

  async fn inject_engine(&self) -> Result<()> {
    // The lazy-inject script is an async IIFE returning a promise.
    // `Runtime.evaluate` has no `awaitPromise`; route the call through
    // `Runtime.callFunctionOn` (which does) anchored on the global, so
    // we synchronously block until `window.__fd` is live.
    let js = crate::selectors::build_lazy_inject_js();
    let wrapper = format!("function(){{ return ({js}); }}");
    // Boxed: `call_on_global` may call back into this function.
    let resp = Box::pin(self.call_on_global(
      json!({
        "functionDeclaration": wrapper,
        "returnByValue": false,
        "awaitPromise": true,
      }),
      false,
    ))
    .await?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let text = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("engine injection threw")
        .to_string();
      return Err(FerriError::evaluation(text));
    }
    Ok(())
  }

  /// Block until the page has painted one frame. Wheel events hit the
  /// compositor first, so dispatching before it syncs silently drops
  /// the scroll (mirrors `wkInput.ts::wheel`'s
  /// `new Promise(requestAnimationFrame)` round-trip). Uses
  /// `Runtime.callFunctionOn` because `WebKit`'s `Runtime.evaluate` has
  /// no `awaitPromise`.
  pub(crate) async fn wait_for_compositor_frame(&self) -> Result<()> {
    self
      .call_on_global(
        json!({
          "functionDeclaration":
            "function(){ return new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); }",
          "returnByValue": true,
          "awaitPromise": true,
        }),
        false,
      )
      .await?;
    Ok(())
  }

  /// Resolve the global-object `objectId` for a specific execution
  /// context (an iframe's realm), evaluating `"this"` with `contextId`
  /// set. Cached per context in [`Self::frame_engine_contexts`].
  pub(super) async fn frame_context_anchor(&self, context_id: i64) -> Result<String> {
    let cached = self
      .frame_engine_contexts
      .read()
      .await
      .get(&context_id)
      .and_then(|c| c.global_object_id.clone());
    if let Some(id) = cached {
      return Ok(id);
    }
    let resp = self
      .target_session()
      .send(
        protocol::RUNTIME_EVALUATE,
        json!({ "expression": "this", "returnByValue": false, "contextId": context_id }),
      )
      .await
      .map_err(conn_err)?;
    let id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "frame context anchor: no objectId"))?
      .to_string();
    self
      .frame_engine_contexts
      .write()
      .await
      .entry(context_id)
      .or_default()
      .global_object_id = Some(id.clone());
    Ok(id)
  }

  /// Inject the `window.__fd` selector engine into the iframe realm
  /// identified by `context_id`. Idempotent per context. Mirrors
  /// [`Self::ensure_engine_injected`] but anchors `callFunctionOn` on
  /// the child realm's global so `__fd` lands in THAT realm. Playwright
  /// creates one `FrameExecutionContext` (and lazily injects the
  /// utility script) per `Runtime.executionContextCreated`
  /// (`wkPage.ts::_onExecutionContextCreated`).
  pub(super) async fn ensure_engine_in_context(&self, context_id: i64) -> Result<()> {
    if self
      .frame_engine_contexts
      .read()
      .await
      .get(&context_id)
      .is_some_and(|c| c.engine_injected)
    {
      return Ok(());
    }
    // Prune engine entries for contexts that have since been replaced
    // (a navigated frame gets a fresh contextId) so the map stays
    // bounded across long-lived pages.
    {
      let live: rustc_hash::FxHashSet<i64> = self.frame_contexts.read().await.values().copied().collect();
      self
        .frame_engine_contexts
        .write()
        .await
        .retain(|id, _| *id == context_id || live.contains(id));
    }
    let anchor = self.frame_context_anchor(context_id).await?;
    let js = crate::selectors::build_lazy_inject_js();
    let wrapper = format!("function(){{ return ({js}); }}");
    let resp = self
      .call_function_on(json!({
        "objectId": anchor,
        "functionDeclaration": wrapper,
        "returnByValue": false,
        "awaitPromise": true,
      }))
      .await
      .map_err(conn_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let text = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("frame engine injection threw")
        .to_string();
      return Err(FerriError::evaluation(text));
    }
    self
      .frame_engine_contexts
      .write()
      .await
      .entry(context_id)
      .or_default()
      .engine_injected = true;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when resolving the main-world global with `Runtime.evaluate` or the
  /// `Runtime.callFunctionOn` evaluation fails, or `FerriError::Evaluation` when `expression`
  /// throws.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<Value>> {
    Ok(Some(self.eval_value(expression).await?))
  }

  // ── Elements ──────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when injecting the selector engine fails, `FerriError::InvalidSelector` when
  /// `selector` does not parse, and `FerriError::ElementNotFound` when evaluating it yields no
  /// element or fails for any other reason.
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    self.ensure_engine_injected().await?;
    let sel_js = crate::selectors::build_selone_js(selector, "window.__fd", false)?;
    self
      .evaluate_to_element(&sel_js, None)
      .await
      .map_err(|_| FerriError::element_not_found(selector))
  }

  /// # Errors
  ///
  /// Returns an error when injecting the selector engine into the frame's context or the
  /// `Runtime.evaluate` command fails, `FerriError::Evaluation` when `js` throws, and
  /// `FerriError::Protocol` when the result is not an object.
  pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement> {
    // Resolve element selectors inside the owning frame's realm so the
    // returned `objectId` is bound to the right execution context and
    // `window.__fd` is present there. `None` (or the main frame) uses
    // the page's default realm.
    let ctx_id = match frame_id {
      Some(fid) => self.frame_contexts.read().await.get(fid).copied(),
      None => None,
    };
    let resp = if let Some(id) = ctx_id {
      if js.contains("window.__fd") {
        self.ensure_engine_in_context(id).await?;
      }
      let r = self
        .target_session()
        .send(
          protocol::RUNTIME_EVALUATE,
          json!({ "expression": js, "returnByValue": false, "contextId": id }),
        )
        .await
        .map_err(conn_err)?;
      if r.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
        let text = r
          .get("result")
          .and_then(|res| res.get("description").or_else(|| res.get("value")))
          .and_then(Value::as_str)
          .unwrap_or("evaluation threw")
          .to_string();
        return Err(FerriError::evaluation(text));
      }
      r
    } else {
      self.runtime_evaluate(js, false).await?
    };
    let object_id = resp
      .get("result")
      .and_then(|r| r.get("objectId"))
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("Runtime.evaluate", "evaluate_to_element: result is not an object"))?;
    Ok(AnyElement::WebKit(WebKitElement::new(
      self.target_session(),
      object_id.to_string(),
    )))
  }

  /// # Errors
  ///
  /// Never fails; the `Result` matches the other backends' signature.
  pub async fn resolve_backend_node(&self, _backend_node_id: i64, ref_id: &str) -> Result<AnyElement> {
    tokio::task::yield_now().await;
    Ok(AnyElement::WebKit(WebKitElement::new(
      self.target_session(),
      ref_id.to_string(),
    )))
  }

  // ── Content ───────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when resolving the main-world global with `Runtime.evaluate` or evaluating
  /// `document.documentElement.outerHTML` with `Runtime.callFunctionOn` fails, or
  /// `FerriError::Evaluation` when it throws.
  pub async fn content(&self) -> Result<String> {
    Ok(
      self
        .eval_value("document.documentElement.outerHTML")
        .await?
        .as_str()
        .unwrap_or_default()
        .to_string(),
    )
  }

  /// # Errors
  ///
  /// Returns an error when the `Runtime.evaluate` command that writes `html` into the document
  /// fails, or `FerriError::Evaluation` when it throws.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    let escaped = serde_json::to_string(html).unwrap_or_else(|_| "\"\"".into());
    self
      .runtime_evaluate(
        &format!("(()=>{{document.open();document.write({escaped});document.close();}})()"),
        true,
      )
      .await?;
    Ok(())
  }

  /// Write `html` into the document and return the `readyState`/iframe probe
  /// in the same `Runtime.evaluate` round trip.
  ///
  /// # Errors
  ///
  /// Returns an error when `Runtime.evaluate` fails or throws.
  pub async fn set_content_and_check(&self, html: &str, dom_content_loaded: bool) -> Result<Option<serde_json::Value>> {
    let escaped = serde_json::to_string(html).unwrap_or_else(|_| "\"\"".into());
    let tail = if dom_content_loaded {
      "const s=document.readyState;return (s==='interactive'||s==='complete')?(document.querySelector('iframe,frame')?2:1):0;"
    } else {
      "return document.readyState==='complete'?(document.querySelector('iframe,frame')?2:1):0;"
    };
    self
      .runtime_evaluate(
        &format!("(()=>{{document.open();document.write({escaped});document.close();{tail}}})()"),
        true,
      )
      .await
      .map(Some)
  }

  // ── Handle / utility evaluate ─────────────────────────────────────────

  /// ferridriver's analogue of Playwright's `evaluateExpression` — runs
  /// the shared `UTILITY_EVAL_WRAPPER` against the page's `UtilityScript`.
  /// `Runtime.callFunctionOn` (which has `awaitPromise`) anchored on the
  /// main-world global, or on the first handle's `objectId` when handles
  /// are supplied (free context anchor, same realm).
  ///
  /// # Errors
  ///
  /// Returns `FerriError::InvalidArgument` for a handle from another backend,
  /// `FerriError::Evaluation` when the function throws, and an error when injecting the selector
  /// engine, resolving the frame's execution context, the `Runtime.callFunctionOn` call, or parsing
  /// its reply fails. A call lost to a replaced document is retried up to `ENGINE_ATTEMPTS` times
  /// first.
  pub async fn call_utility_evaluate(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    use crate::backend::cdp::{ENGINE_ATTEMPTS, ENGINE_MISSING};
    let mut attempt = 1;
    loop {
      let result =
        Box::pin(self.call_utility_evaluate_once(fn_source, args, handles, frame_id, is_function, return_by_value))
          .await;
      match result {
        Err(error) if attempt < ENGINE_ATTEMPTS && error.to_string().contains(ENGINE_MISSING) => {
          // The document the engine went into was replaced before the
          // call ran; nothing of the caller's has run yet.
          self.engine_injected.reset();
          self.frame_engine_contexts.write().await.clear();
          attempt += 1;
        },
        result => return result,
      }
    }
  }

  async fn call_utility_evaluate_once(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    frame_id: Option<&str>,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    self.ensure_engine_injected().await?;

    // Frame-scoped evaluate: resolve the frame's execution context and
    // anchor the wrapper there via a per-context `this` objectId (the
    // cached `global_anchor` is main-frame-only).
    let frame_ctx_id = match frame_id {
      Some(fid) => Some(self.resolve_frame_context(fid).await?),
      None => None,
    };

    let args_json = serde_json::to_string(args)?;
    let is_fn: Value = match is_function {
      Some(b) => Value::Bool(b),
      None => Value::Null,
    };
    let count = args.len();
    let mut arguments: Vec<Value> = vec![
      json!({ "value": is_fn }),
      json!({ "value": return_by_value }),
      json!({ "value": fn_source }),
      json!({ "value": count }),
      json!({ "value": args_json }),
    ];
    let mut anchor: Option<String> = None;
    for handle in handles {
      match handle {
        crate::protocol::HandleId::WebKit(obj) => {
          if anchor.is_none() {
            anchor = Some(obj.clone());
          }
          arguments.push(json!({ "objectId": obj }));
        },
        other => {
          return Err(FerriError::invalid_argument(
            "handles",
            format!("call_utility_evaluate: non-webkit handle {other:?} on webkit backend"),
          ));
        },
      }
    }
    let anchor = match anchor {
      Some(a) => {
        // The wrapper runs in the realm the anchor handle belongs to.
        // A handle resolved in a child frame carries that frame's
        // `injectedScriptId` (== execution context id) inside its
        // objectId JSON; `window.__fd` must exist in THAT realm for the
        // UtilityScript wrapper to work. Inject it there if needed.
        if let Some(ctx_id) = object_id_context(&a)
          && self.frame_contexts.read().await.values().any(|&v| v == ctx_id)
        {
          self.ensure_engine_in_context(ctx_id).await?;
        }
        Some(a)
      },
      None => match frame_ctx_id {
        Some(ctx_id) => {
          self.ensure_engine_in_context(ctx_id).await?;
          Some(self.anchor_in_context(ctx_id).await?)
        },
        None => None,
      },
    };
    let params = json!({
      "functionDeclaration": crate::backend::cdp::UTILITY_EVAL_WRAPPER,
      "arguments": arguments,
      "returnByValue": return_by_value,
      "awaitPromise": true,
    });
    let resp = match anchor {
      Some(anchor) => {
        let mut params = params;
        params["objectId"] = Value::String(anchor);
        self.call_function_on(params).await.map_err(conn_err)?
      },
      None => self.call_on_global(params, true).await?,
    };
    parse_eval_response(&resp, return_by_value)
  }

  /// # Errors
  ///
  /// Never fails; a failed `Runtime.releaseObject` is ignored and the `Result` matches the other
  /// backends' signature.
  pub async fn release_object(&self, object_id: &str) -> Result<()> {
    let _ = self
      .target_session()
      .send(protocol::RUNTIME_RELEASE_OBJECT, json!({ "objectId": object_id }))
      .await;
    Ok(())
  }

  #[must_use]
  pub fn element_from_object_id(&self, object_id: String) -> WebKitElement {
    WebKitElement::new(self.target_session(), object_id)
  }
}
