//! `BidiPage` JavaScript evaluation, elements, content, utility eval, and handle disposal.

use super::{AnyElement, Arc, BidiElement, BidiPage, EvaluateResult, FerriError, Result, json};

impl BidiPage {
  // ── JavaScript ──────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` that injects the selector engine into the main
  /// context fails.
  pub async fn injected_script(&self) -> Result<String> {
    self.ensure_engine_injected().await?;
    Ok("window.__fd".to_string())
  }

  /// Ensures the selector engine is injected into the current execution context.
  /// Idempotent and navigation-aware.
  ///
  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` that injects the selector engine into the main
  /// context fails.
  pub async fn ensure_engine_injected(&self) -> Result<()> {
    self.injected_script.ensure(self).await
  }

  /// See [`crate::backend::AnyPage::forget_engine`].
  pub fn forget_engine(&self, frame_id: Option<&str>) {
    self.injected_script.reset_context(frame_id.unwrap_or(&self.context_id));
  }

  /// Ensure the engine is injected into a specific browsing context.
  /// Used before frame-scoped queries so freshly-attached / `srcdoc` /
  /// `data:` child contexts have `window.__fd` (Residual 2).
  ///
  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` that injects the selector engine into `ctx` fails.
  pub async fn ensure_engine_injected_in(&self, ctx: &str) -> Result<()> {
    self.injected_script.ensure_in(self, ctx).await
  }

  /// Block until `ctx` has a usable document, then return. Fast-path:
  /// `document.readyState` is `interactive`/`complete` (true for
  /// `srcdoc` / `data:` children, which parse synchronously) — returns
  /// immediately, no sleep. Otherwise wait on the real `BiDi`
  /// `browsingContext.domContentLoaded` / `load` event for that context
  /// (no fixed delay), bounded so a wedged child can't hang a query.
  pub(super) async fn wait_context_ready(&self, ctx: &str) -> Result<()> {
    // Subscribe before looking: a child that finishes parsing between the
    // readiness check and the subscription would otherwise fire its
    // event unseen and leave this waiting out the whole bound.
    let mut rx = self.session.transport.tap_events();
    // Fast path: query readiness directly. A successful evaluate also
    // proves the context is reachable. A probe the operation's deadline
    // cut off says nothing about the child, and the operation is over.
    match self.eval_internal("document.readyState", ctx).await {
      Ok(Some(v)) if v.as_str().is_some_and(|s| s == "interactive" || s == "complete") => return Ok(()),
      Err(error) if error.is_timeout_error() => return Err(error),
      _ => {},
    }
    // Slow path: the child is still parsing — wait for its own
    // domContentLoaded/load event (BiDi reports child events with
    // `context` == the child id), for no longer than the operation has.
    let target = ctx.to_string();
    let budget = crate::operation_budget::OperationBudget::current(30_000)?;
    let waited = budget
      .wait(async move {
        while let Some(event) = rx.recv().await {
          if matches!(
            event.method.as_str(),
            "browsingContext.domContentLoaded" | "browsingContext.load"
          ) && event.params.get("context").and_then(|v| v.as_str()) == Some(target.as_str())
          {
            return;
          }
        }
      })
      .await;
    // A timeout here is not fatal — the caller's own retry/deadline
    // still governs; we just stop blocking. The subsequent query will
    // surface a precise "no element found" if the child never loaded.
    let _ = waited;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` command fails or its result cannot be parsed, and
  /// `FerriError::Evaluation` when the expression throws.
  pub async fn evaluate(&self, expression: &str) -> Result<Option<serde_json::Value>> {
    self.eval_internal(expression, &self.context_id).await
  }

  // ── Elements ────────────────────────────────────────────────────────────

  /// Stable page identity for request attribution — the top-level
  /// browsing-context id, same value the network tracker stamps on
  /// captured requests.
  #[must_use]
  pub fn page_guid(&self) -> String {
    self.context_id.to_string()
  }

  /// # Errors
  ///
  /// Returns an error when the selector engine cannot be injected or `selector` cannot be parsed,
  /// and `FerriError::ElementNotFound` when resolving the element fails for any reason.
  pub async fn find_element(&self, selector: &str) -> Result<AnyElement> {
    self.ensure_engine_injected().await?;
    // `find_element` is a non-strict path (used by raw page.locator
    // resolution + a few engine-internal callers); pass strict=false
    // so the engine returns first-match instead of throwing on
    // multi-match.
    let sel_js = crate::selectors::build_selone_js(selector, "window.__fd", false)?;
    self
      .evaluate_to_element(&sel_js, None)
      .await
      .map_err(|_| FerriError::element_not_found(selector))
  }

  /// `BiDi`: a "frame" is a browsing context. When `frame_id` is `Some`
  /// it overrides the page's default `context_id` so element resolution
  /// runs inside the iframe's realm. Mirrors Playwright's `BiDi` backend
  /// (`/tmp/playwright/packages/playwright-core/src/server/bidi/bidiFrame.ts`).
  ///
  /// # Errors
  ///
  /// Returns an error when `script.callFunction` or `script.evaluate` fails or its result is not a
  /// DOM node, and `FerriError::Evaluation` when the script throws. For a child frame it also fails
  /// when injecting the selector engine fails or the operation's deadline passes while the frame
  /// loads.
  pub async fn evaluate_to_element(&self, js: &str, frame_id: Option<&str>) -> Result<AnyElement> {
    // The JS can be either an expression or a function.
    // Use script.evaluate for expressions, script.callFunction for functions.
    let is_function = js.trim_start().starts_with("function") || js.trim_start().starts_with('(');

    let target_ctx: &str = frame_id.unwrap_or(&self.context_id);

    // Frame-scoped resolution runs `window.__fd.selOne(...)` inside the
    // child browsing context. Ensure the engine is present there (and
    // the child is parseable) before querying — covers freshly-attached
    // / `srcdoc` / `data:` children (Residual 2). Main context is
    // already injected by the caller's `ensure_engine_injected`.
    if let Some(fid) = frame_id
      && fid != &*self.context_id
    {
      self.wait_context_ready(fid).await?;
      self.ensure_engine_injected_in(fid).await?;
    }

    let result = if is_function {
      self
        .cmd(
          "script.callFunction",
          json!({
            "functionDeclaration": js,
            "target": {"context": target_ctx},
            "awaitPromise": true,
            "resultOwnership": "root"
          }),
        )
        .await?
    } else {
      self
        .cmd(
          "script.evaluate",
          json!({
            "expression": js,
            "target": {"context": target_ctx},
            "awaitPromise": true,
            "resultOwnership": "root"
          }),
        )
        .await?
    };

    let eval_result: EvaluateResult = serde_json::from_value(result)
      .map_err(|e| FerriError::protocol("script.callFunction", format!("BiDi evaluate_to_element parse: {e}")))?;

    match eval_result {
      EvaluateResult::Success { result: remote_val } => {
        let shared_ref = remote_val.as_shared_reference().ok_or_else(|| {
          FerriError::protocol("script.callFunction", "evaluate_to_element: result is not a DOM node")
        })?;
        // Element belongs to the realm we evaluated in.
        let owning_ctx: Arc<str> = match frame_id {
          Some(fid) => Arc::from(fid),
          None => Arc::clone(&self.context_id),
        };
        // Record the realm so that when this element is repackaged as a
        // `JSHandle`/`ElementHandle` and later evaluated against (with
        // `frame_id = None`), `call_utility_evaluate` re-targets the
        // owning child context instead of the page's main context.
        self.remember_handle_realm(&shared_ref.shared_id, &owning_ctx);
        Ok(AnyElement::Bidi(BidiElement::new(
          Arc::clone(&self.session),
          owning_ctx,
          shared_ref.shared_id,
        )))
      },
      EvaluateResult::Exception { exception_details } => Err(FerriError::evaluation(format!(
        "JS error in evaluate_to_element: {}",
        exception_details.text
      ))),
    }
  }

  // ── Content ─────────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the `script.evaluate` of `document.documentElement.outerHTML` fails or
  /// its result cannot be parsed, and `FerriError::Evaluation` when it throws.
  pub async fn content(&self) -> Result<String> {
    let result = self
      .eval_internal("document.documentElement.outerHTML", &self.context_id)
      .await?;
    Ok(result.and_then(|v| v.as_str().map(String::from)).unwrap_or_default())
  }

  /// # Errors
  ///
  /// Returns an error when the `script.callFunction` command that writes `html` into the document
  /// fails.
  pub async fn set_content(&self, html: &str) -> Result<()> {
    self
      .cmd(
        "script.callFunction",
        json!({
          "functionDeclaration": "(html) => { document.open(); document.write(html); document.close(); }",
          "target": {"context": &*self.context_id},
          "arguments": [{"type": "string", "value": html}],
          "awaitPromise": false,
          "resultOwnership": "none"
        }),
      )
      .await?;
    Ok(())
  }

  /// Write `html` into the document and evaluate the `readyState`/iframe check
  /// inside the same `script.callFunction` round trip.
  ///
  /// # Errors
  ///
  /// Returns an error when `script.callFunction` fails.
  pub async fn set_content_and_check(&self, html: &str, dom_content_loaded: bool) -> Result<Option<serde_json::Value>> {
    let decl = if dom_content_loaded {
      "(html) => { document.open(); document.write(html); document.close(); const s = document.readyState; return (s === 'interactive' || s === 'complete') ? (document.querySelector('iframe,frame') ? 2 : 1) : 0; }"
    } else {
      "(html) => { document.open(); document.write(html); document.close(); return document.readyState === 'complete' ? (document.querySelector('iframe,frame') ? 2 : 1) : 0; }"
    };
    let result = self
      .cmd(
        "script.callFunction",
        json!({
          "functionDeclaration": decl,
          "target": {"context": &*self.context_id},
          "arguments": [{"type": "string", "value": html}],
          "awaitPromise": false,
          "resultOwnership": "none"
        }),
      )
      .await?;
    Ok(result.pointer("/result/value").cloned())
  }

  // ── Utility-script evaluate (Playwright `page.evaluate(fn, arg)`) ──────

  /// Call the page-side `UtilityScript.evaluate` over `BiDi`. Parallels
  /// `CdpPage::call_utility_evaluate` but sends the arguments as
  /// `BiDi` `LocalValue` / `RemoteReference` entries through
  /// `script.callFunction`.
  ///
  /// The wrapper function is identical to the CDP path — it memoises
  /// the utility script on `window.__fd.__us`, `JSON.parse`s the
  /// serialized argument, and `JSON.stringify`s the result back so we
  /// only ship strings through `script.callFunction`'s native
  /// serializer. Handles arrive as `{type: "sharedReference", sharedId}`
  /// `BiDi` arguments — the browser hydrates each back to its native
  /// JS object before the wrapper receives it.
  ///
  /// # Errors
  ///
  /// Returns a String on transport failure, page-side exception, or
  /// handle/backend mismatch.
  /// Construct a [`super::super::BidiElement`] directly from a shared-reference
  /// id without re-resolving through the DOM. Used by
  /// [`crate::backend::element_from_remote`] when a
  /// [`crate::js_handle::JSHandle`] turns out to wrap a DOM node and
  /// needs to be re-packaged as an
  /// [`crate::element_handle::ElementHandle`].
  pub(crate) fn element_from_shared_id(&self, shared_id: String) -> super::super::BidiElement {
    super::super::BidiElement::new(Arc::clone(&self.session), Arc::clone(&self.context_id), shared_id)
  }

  /// ferridriver's equivalent of Playwright's
  /// `evaluateExpression(context, expr, opts, ...args)` — see the CDP
  /// twin for the shared contract. Sends variadic `args` + shared
  /// `handles` through `script.callFunction`.
  ///
  /// # Errors
  ///
  /// Returns a String on transport failure, page-side exception, or
  /// handle/backend mismatch.
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
          self.injected_script.reset();
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
    use crate::protocol::HandleId;
    use serde_json::json;

    self.ensure_engine_injected().await?;

    // Resolve the realm to evaluate in. An explicit `frame_id` (frame-
    // scoped evaluate) wins. Otherwise — the `JSHandle::evaluate` /
    // `ElementHandle` path the shared core dispatches with `frame_id =
    // None` — recover the realm from a handle argument: a handle
    // retained in a child frame is realm-scoped and only resolves when
    // `target` is that same browsing context.
    let resolved_ctx: Option<Arc<str>> = if frame_id.is_some() {
      None
    } else {
      handles.iter().find_map(|h| match h {
        HandleId::Bidi { shared_id, handle } => handle
          .as_deref()
          .and_then(|hh| self.handle_realm(hh))
          .or_else(|| self.handle_realm(shared_id)),
        _ => None,
      })
    };
    let target_ctx: &str = frame_id.or(resolved_ctx.as_deref()).unwrap_or(&self.context_id);

    // A utility eval targeting a child browsing context (e.g. the
    // recursive cross-iframe `ariaSnapshot` stitch running
    // `window.__fd.ariaSnapshotFrame(document.body, ...)` inside
    // an iframe) needs the engine present in that context, and the
    // child to be parseable. Same chokepoint as `evaluate_to_element`
    // / `evaluate_in_frame` (Residual 2) — main context is already
    // injected above.
    if target_ctx != &*self.context_id {
      self.wait_context_ready(target_ctx).await?;
      self.ensure_engine_injected_in(target_ctx).await?;
    }

    Box::pin(self.call_utility_in_target(
      fn_source,
      args,
      handles,
      json!({"context":target_ctx}),
      is_function,
      return_by_value,
    ))
    .await
  }

  pub(crate) async fn evaluate_isolated(
    &self,
    source: &str,
    arg: &crate::protocol::SerializedArgument,
    frame_id: &str,
  ) -> Result<crate::protocol::SerializedValue> {
    let target = json!({"context":frame_id,"sandbox":"__ferridriver_webmcp"});
    let injected = self
      .cmd(
        "script.evaluate",
        json!({
          "expression":crate::selectors::UTILITY_SCRIPT_JS,
          "target":&target, "awaitPromise":true, "resultOwnership":"none",
        }),
      )
      .await?;
    let injected: super::super::types::EvaluateResult = serde_json::from_value(injected)?;
    if let super::super::types::EvaluateResult::Exception { exception_details } = injected {
      return Err(FerriError::evaluation(exception_details.text));
    }
    match self
      .call_utility_in_target(
        source,
        std::slice::from_ref(&arg.value),
        &arg.handles,
        target,
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

  async fn call_utility_in_target(
    &self,
    fn_source: &str,
    args: &[crate::protocol::SerializedValue],
    handles: &[crate::protocol::HandleId],
    target: serde_json::Value,
    is_function: Option<bool>,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    use crate::protocol::HandleId;
    let target_ctx = target["context"]
      .as_str()
      .ok_or_else(|| FerriError::protocol("utility evaluation", "missing browsing context"))?
      .to_owned();
    let args_json = serde_json::to_string(args)?;
    let count = args.len();

    let is_fn_local = match is_function {
      Some(true) => json!({"type": "boolean", "value": true}),
      Some(false) => json!({"type": "boolean", "value": false}),
      None => json!({"type": "undefined"}),
    };

    let mut arguments = vec![
      is_fn_local,
      json!({"type": "boolean", "value": return_by_value}),
      json!({"type": "string", "value": fn_source}),
      json!({"type": "number", "value": count}),
      json!({"type": "string", "value": args_json}),
    ];
    for handle in handles {
      match handle {
        HandleId::Bidi { shared_id, handle } => {
          // BiDi has two distinct handle shapes: `sharedReference`
          // is DOM-node-only (cross-context node identity via UUID)
          // and `handle` is any retained remote (Object, Array,
          // Function, Map, Set, etc.) via the per-session handle
          // registry. Prefer `handle` when present — it's the more
          // general form that also works for nodes inside a single
          // context. Only fall back to `sharedReference` when the
          // remote is a node without a retained handle.
          if let Some(h) = handle {
            arguments.push(json!({"type": "handle", "handle": h}));
          } else if !shared_id.is_empty() {
            arguments.push(json!({"type": "sharedReference", "sharedId": shared_id}));
          } else {
            return Err(FerriError::invalid_argument(
              "handles",
              "BiDi handle carries neither sharedId nor handle",
            ));
          }
        },
        _ => {
          return Err(FerriError::invalid_argument(
            "handles",
            "call_utility_evaluate: non-BiDi handle in arg.handles on BiDi backend",
          ));
        },
      }
    }

    let params = json!({
      "functionDeclaration": crate::backend::cdp::UTILITY_EVAL_WRAPPER,
      "target": target,
      "arguments": arguments,
      "awaitPromise": true,
      "resultOwnership": if return_by_value { "none" } else { "root" },
    });

    let response = self.cmd("script.callFunction", params).await?;
    let eval_result: super::super::types::EvaluateResult = serde_json::from_value(response)
      .map_err(|e| FerriError::Backend(format!("BiDi call_utility_evaluate parse: {e}")))?;

    self.parse_utility_result(eval_result, &target_ctx, return_by_value)
  }

  fn parse_utility_result(
    &self,
    eval_result: super::super::types::EvaluateResult,
    target_ctx: &str,
    return_by_value: bool,
  ) -> Result<crate::js_handle::EvaluateResult> {
    use crate::js_handle::{EvaluateResult as FdEvalResult, HandleRemote};
    match eval_result {
      super::super::types::EvaluateResult::Exception { exception_details } => Err(FerriError::evaluation(format!(
        "Evaluation error: {}",
        exception_details.text
      ))),
      super::super::types::EvaluateResult::Success { result } => {
        if return_by_value {
          // Wrapper JSON.stringified its result. The BiDi wire shape
          // for a string is `{type: "string", value: "<json>"}`; for
          // a null (undefined sentinel) it's `{type: "null"}`.
          let inner_json: serde_json::Value = match result {
            super::super::types::RemoteValue::String { value } => {
              let s = value.as_str().unwrap_or("null").to_string();
              serde_json::from_str(&s).map_err(|e| FerriError::Backend(format!("BiDi parse utility result: {e}")))?
            },
            super::super::types::RemoteValue::Null | super::super::types::RemoteValue::Undefined => {
              return Ok(FdEvalResult::Value(crate::protocol::SerializedValue::Special(
                crate::protocol::SpecialValue::Undefined,
              )));
            },
            other => {
              return Err(FerriError::Backend(format!(
                "BiDi call_utility_evaluate: wrapper returned non-string in returnByValue mode: {other:?}"
              )));
            },
          };
          let parsed: crate::protocol::SerializedValue = serde_json::from_value(inner_json)
            .map_err(|e| FerriError::Backend(format!("BiDi parse SerializedValue: {e}")))?;
          Ok(FdEvalResult::Value(parsed))
        } else {
          // Returning a handle: the result is either a DOM node
          // (sharedReference), a non-node object (handle field), or
          // a primitive (inline value — no page-side retention).
          // Playwright's `JSHandle` has both shapes; mirror that here.
          if let Some(shared) = result.as_shared_reference() {
            // sharedReference is BiDi's DOM-node shape. Remember which
            // realm it came from so a later `JSHandle::evaluate`
            // (dispatched with `frame_id = None`) re-targets it.
            self.remember_handle_realm(&shared.shared_id, target_ctx);
            if let Some(h) = shared.handle.as_deref() {
              self.remember_handle_realm(h, target_ctx);
            }
            Ok(FdEvalResult::Handle(
              crate::js_handle::JSHandleBacking::Remote(HandleRemote::Bidi {
                shared_id: shared.shared_id,
                handle: shared.handle,
              }),
              true,
            ))
          } else {
            let non_node_handle = match &result {
              super::super::types::RemoteValue::Array { handle, .. }
              | super::super::types::RemoteValue::Object { handle, .. }
              | super::super::types::RemoteValue::Map { handle, .. }
              | super::super::types::RemoteValue::Set { handle, .. }
              | super::super::types::RemoteValue::Function { handle }
              | super::super::types::RemoteValue::Error { handle }
              | super::super::types::RemoteValue::Promise { handle }
              | super::super::types::RemoteValue::Symbol { handle } => handle.clone(),
              _ => None,
            };
            if let Some(h) = non_node_handle {
              // Store the BiDi handle ONLY in the `handle` slot —
              // `shared_id` is reserved for DOM-node cross-context
              // references. A later argument-pass emits this remote
              // as `{type: "handle", handle}`, which BiDi accepts
              // for any retained Object / Array / Function / ...
              // remote. Emitting it as `{type: "sharedReference",
              // sharedId}` (the old mistake) causes BiDi to reject
              // with "no such node" because the handle-string was
              // never registered as a node sharedId.
              self.remember_handle_realm(&h, target_ctx);
              Ok(FdEvalResult::Handle(
                crate::js_handle::JSHandleBacking::Remote(HandleRemote::Bidi {
                  shared_id: String::new(),
                  handle: Some(h),
                }),
                false,
              ))
            } else {
              // Primitive result from evaluateHandle — wrap the
              // inline value as a value-backed JSHandle. BiDi maps
              // `Undefined`, `Null`, `Boolean`, `Number`, `String`,
              // `BigInt` to native JSON (plus a BigInt decimal
              // string) via `RemoteValue::to_json`. The one BiDi
              // shape that has no JSON projection (Symbol without a
              // handle) already falls into the non-node branch above.
              let as_json = result.to_json().unwrap_or(serde_json::Value::Null);
              let serialized = match &result {
                super::super::types::RemoteValue::Undefined => {
                  crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Undefined)
                },
                super::super::types::RemoteValue::Null => {
                  crate::protocol::SerializedValue::Special(crate::protocol::SpecialValue::Null)
                },
                super::super::types::RemoteValue::BigInt { value } => {
                  let s = value
                    .as_str()
                    .map_or_else(|| value.to_string(), std::string::ToString::to_string);
                  crate::protocol::SerializedValue::BigInt(s)
                },
                _ => {
                  let mut ctx = crate::protocol::SerializationContext::default();
                  crate::protocol::SerializedValue::from_json(&as_json, &mut ctx)
                },
              };
              Ok(FdEvalResult::Handle(
                crate::js_handle::JSHandleBacking::Value(serialized),
                false,
              ))
            }
          }
        }
      },
    }
  }

  // ── Handle lifecycle ────────────────────────────────────────────────────

  /// Release the remote handle identified by `shared_id` via
  /// `script.disown`. Used by `AnyPage::release_handle` when disposing a
  /// `JSHandle` / `ElementHandle` on a `BiDi` backend.
  ///
  /// `BiDi`'s `script.disown` takes an array of handle strings scoped to
  /// one target; we always pass a single `sharedId`. If `handle` is
  /// `Some(_)`, it is used verbatim — some browsers supply an
  /// implementation-specific `handle` alongside `sharedId` that
  /// `script.disown` accepts; the `sharedId` form is universally
  /// supported and is what Playwright's own `bidiSession.ts` uses.
  ///
  /// # Errors
  ///
  /// Returns the transport error if the `BiDi` command fails.
  pub async fn release_handle(&self, shared_id: &str, handle: Option<&str>) -> Result<()> {
    let handle_str = handle.unwrap_or(shared_id);
    // Disown in the realm the reference was created in — a child-frame
    // handle is realm-scoped and `script.disown` against the main
    // context would not match it.
    let ctx = handle
      .and_then(|h| self.handle_realm(h))
      .or_else(|| self.handle_realm(shared_id))
      .unwrap_or_else(|| Arc::clone(&self.context_id));
    let result = self
      .cmd(
        "script.disown",
        json!({
          "handles": [handle_str],
          "target": {"context": &*ctx},
        }),
      )
      .await
      .map(|_| ());
    self.forget_handle_realm(shared_id);
    if let Some(h) = handle {
      self.forget_handle_realm(h);
    }
    result
  }
}
