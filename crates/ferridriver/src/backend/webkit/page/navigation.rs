//! `WebKitPage` frames and navigation.

use super::{
  Arc, FerriError, FrameInfo, NavLifecycle, NavigateParams, NavigateResult, Ordering, Response, Result, Value,
  WebKitPage, collect_frames, conn_err, json, protocol,
};

impl WebKitPage {
  // ── Frames ────────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the `Page.getResourceTree` command fails.
  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>> {
    let resp = self
      .target_session()
      .send("Page.getResourceTree", json!({}))
      .await
      .map_err(conn_err)?;
    let mut frames = Vec::new();
    if let Some(root) = resp.get("frameTree") {
      collect_frames(root, None, &mut frames);
    }
    Ok(frames)
  }

  /// The content-frame id hosted inside an `<iframe>`/`<frame>` element
  /// given its remote-object id, via `DOM.describeNode` →
  /// `contentFrameId` (mirrors `wkPage.ts::getContentFrame`). `None`
  /// when the element hosts no frame. Deterministic — works for
  /// unnamed / `srcdoc` / `data:` iframes the name/url cache heuristic
  /// cannot attribute.
  ///
  /// # Errors
  ///
  /// Returns an error when the `DOM.describeNode` command fails.
  pub async fn content_frame_id(&self, object_id: &str) -> Result<Option<String>> {
    let resp = self
      .target_session()
      .send(protocol::DOM_DESCRIBE_NODE, json!({ "objectId": object_id }))
      .await
      .map_err(conn_err)?;
    // WebKit nests the node under `node`; the frame id is
    // `contentFrameId` (top-level) in the DOM domain reply.
    let frame_id = resp
      .get("contentFrameId")
      .or_else(|| resp.pointer("/node/contentFrameId"))
      .and_then(Value::as_str)
      .map(std::string::ToString::to_string);
    Ok(frame_id)
  }

  /// Mark a child frame's owner `<iframe>` element with the child's
  /// frame id via the trace snapshot streamer (mirrors
  /// `wkPage.ts::getFrameElement`: `DOM.resolveNode {frameId}` into the
  /// parent's main-world context, then `Runtime.callFunctionOn` with
  /// the element as `this`).
  ///
  /// # Errors
  ///
  /// Returns an error when the parent frame has no execution context after about two seconds, when
  /// `DOM.resolveNode` fails or resolves no live owner element (`FerriError::Protocol`, also for a
  /// detached frame), or when the `Runtime.callFunctionOn` call fails.
  pub async fn mark_snapshot_iframe(
    &self,
    child_frame_id: &str,
    parent_frame_id: &str,
    streamer_global: &str,
  ) -> Result<()> {
    let ctx_id = self.resolve_frame_context(parent_frame_id).await?;
    let resolved = self
      .target_session()
      .send(
        protocol::DOM_RESOLVE_NODE,
        json!({ "frameId": child_frame_id, "executionContextId": ctx_id }),
      )
      .await
      .map_err(conn_err)?;
    let object = resolved
      .get("object")
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", "frame owner did not resolve to an object"))?;
    if object.get("subtype").and_then(Value::as_str) == Some("null") {
      return Err(FerriError::protocol("DOM.resolveNode", "frame has been detached"));
    }
    let object_id = object
      .get("objectId")
      .and_then(Value::as_str)
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", "frame owner has no objectId"))?
      .to_string();
    let declaration =
      format!("function(frameId) {{ const s = window[{streamer_global:?}]; if (s) s.markIframe(this, frameId); }}");
    let call = self
      .call_function_on(json!({
        "functionDeclaration": declaration,
        "objectId": object_id,
        "arguments": [{ "value": child_frame_id }],
        "returnByValue": true,
      }))
      .await
      .map_err(conn_err);
    let _ = self
      .target_session()
      .send(protocol::RUNTIME_RELEASE_OBJECT, json!({ "objectId": object_id }))
      .await;
    call.map(|_| ())
  }

  /// # Errors
  ///
  /// Returns an error when the page is closed or the `Target.activate` command fails.
  pub async fn bring_to_front(&self) -> Result<()> {
    self.ensure_open()?;
    self
      .proxy
      .send(protocol::TARGET_ACTIVATE, json!({"targetId": &*self.target_id()}))
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the frame has no execution context after about two seconds, when
  /// injecting the selector engine into it fails, or when the `Runtime.callFunctionOn` evaluation
  /// fails, and `FerriError::Evaluation` when `expression` throws.
  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<Value>> {
    let ctx_id = self.resolve_frame_context(frame_id).await?;
    // Selector-engine calls reach a child frame's realm through
    // `contextId`. `window.__fd` only lives in whichever realm the
    // engine was injected into, so inject it into THIS context first
    // when the expression depends on it — otherwise the child realm
    // throws `__fd is undefined` on every poll and the wait wedges.
    if expression.contains("window.__fd") {
      self.ensure_engine_in_context(ctx_id).await?;
    }
    self.eval_value_in_context(expression, Some(ctx_id)).await.map(Some)
  }

  // ── Navigation ────────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the page is closed, when `Playwright.navigate` fails or reports an error
  /// text, or when the main document fails to load. A `FerriError::Timeout` comes back when the
  /// navigate reply or the `lifecycle` event misses `timeout_ms`.
  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referrer: Option<&str>,
  ) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    // Reset lifecycle latches BEFORE issuing the navigate -- the listener
    // marks them as events come in. Survives cross-process target swaps
    // (the listener re-subscribes on `Target.didCommitProvisionalTarget`
    // so events from the new process still feed the latches).
    self.lifecycle.reset();
    let params = NavigateParams {
      url: url.to_string(),
      page_proxy_id: self.proxy_id.to_string(),
      frame_id: None,
      referrer: referrer.map(str::to_string),
    };
    let nav = tokio::time::timeout_at(
      crate::backend::deadline_for(timeout_ms),
      self
        .browser
        .send(protocol::PLAYWRIGHT_NAVIGATE, serde_json::to_value(&params)?),
    )
    .await
    .map_err(|_| FerriError::timeout(format!("navigating to {url}"), timeout_ms))?
    .map_err(conn_err)?;
    let parsed: NavigateResult = serde_json::from_value(nav).unwrap_or_default();
    if let Some(err) = parsed.error_text
      && !err.is_empty()
    {
      return Err(FerriError::backend(format!("webkit navigate: {err}")));
    }
    let grace = if parsed.loader_id.is_some() {
      self
        .wait_for_lifecycle(lifecycle, timeout_ms, parsed.loader_id.as_deref())
        .await?;
      crate::network::NAV_REQUEST_GRACE
    } else {
      // No loaderId: a same-document jump, which requested nothing.
      std::time::Duration::ZERO
    };
    Ok(
      self
        .nav_request_slot
        .final_response_for(parsed.loader_id.as_deref(), grace)
        .await,
    )
  }

  /// Resolve the main-document `Response` captured by the network
  /// listener for the most recent navigation.
  /// `grace` is how long to wait for the document request to be seen:
  /// non-zero only when a new document committed, since the request and
  /// the lifecycle reach us on separate consumers and the request can be
  /// processed second. A same-document navigation issues none.
  async fn await_nav_response(&self, grace: std::time::Duration) -> Option<Response> {
    self.nav_request_slot.final_response(grace).await
  }

  /// Wait for the target-session lifecycle event matching `lifecycle`,
  /// or for `Network.loadingFailed` on the main-document request.
  /// Uses [`LifecycleSignals`] fed by the target listener so the wait
  /// survives cross-process target swaps (listener re-subscribes to
  /// the new target on `Target.didCommitProvisionalTarget`).
  async fn wait_for_lifecycle(&self, lifecycle: NavLifecycle, timeout_ms: u64, loader_id: Option<&str>) -> Result<()> {
    let signals = Arc::clone(&self.lifecycle);
    let nav_slot = self.nav_request_slot.clone();
    let result = tokio::time::timeout_at(crate::backend::deadline_for(timeout_ms), async move {
      loop {
        let notified = signals.notify.notified();
        if signals.seen(lifecycle) && signals.committed(loader_id) {
          return Ok(());
        }
        if let Some(err) = signals.provisional_failure(loader_id) {
          return Err(FerriError::backend(format!("webkit navigate: {err}")));
        }
        if let Some((event_request_id, err)) = signals.failure()
          && let Some(req) = nav_slot.get_for(loader_id)
          && event_request_id == req.id()
        {
          return Err(FerriError::backend(format!("webkit navigate: {err}")));
        }
        notified.await;
      }
    })
    .await;
    match result {
      Ok(inner) => inner,
      // A navigation whose lifecycle never fires is a TimeoutError, not
      // a silent success — Playwright rejects the goto/reload promise
      // when the waitUntil signal misses the deadline.
      Err(_) => Err(FerriError::timeout(
        format!("waiting for navigation lifecycle {lifecycle:?}"),
        timeout_ms,
      )),
    }
  }

  /// # Errors
  ///
  /// Returns an error when the main document fails to load, or a `FerriError::Timeout` when no
  /// `load` event arrives within 30 seconds.
  pub async fn wait_for_navigation(&self) -> Result<()> {
    self.lifecycle.reset();
    self.wait_for_lifecycle(NavLifecycle::Load, 30_000, None).await
  }

  /// Force a GC pass in the page's JS engine. Mirrors Playwright's
  /// `wkPage.requestGC` (`Heap.gc`).
  ///
  /// # Errors
  ///
  /// Returns an error when the page is closed or the `Heap.gc` command fails.
  pub async fn request_gc(&self) -> Result<()> {
    self.ensure_open()?;
    self
      .target_session()
      .send("Heap.gc", json!({}))
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the page is closed, when the `Page.reload` command fails or the reloaded
  /// document fails to load, or a `FerriError::Timeout` when `lifecycle` is not reached within
  /// `timeout_ms`.
  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    self.lifecycle.reset();
    self
      .target_session()
      .send("Page.reload", json!({}))
      .await
      .map_err(conn_err)?;
    self.wait_for_lifecycle(lifecycle, timeout_ms, None).await?;
    Ok(self.await_nav_response(crate::network::NAV_REQUEST_GRACE).await)
  }

  /// # Errors
  ///
  /// Returns an error when the page is closed, when `Page.goBack` fails for a reason other than a
  /// missing history entry (which yields `Ok(None)`), or when the new document fails to load. A
  /// `FerriError::Timeout` comes back when `lifecycle` is not reached within `timeout_ms`.
  pub async fn go_back(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_traverse("Page.goBack", lifecycle, timeout_ms).await
  }

  /// # Errors
  ///
  /// Returns an error when the page is closed, when `Page.goForward` fails for a reason other than
  /// a missing history entry (which yields `Ok(None)`), or when the new document fails to load. A
  /// `FerriError::Timeout` comes back when `lifecycle` is not reached within `timeout_ms`.
  pub async fn go_forward(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.history_traverse("Page.goForward", lifecycle, timeout_ms).await
  }

  async fn history_traverse(&self, method: &str, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<Response>> {
    self.ensure_open()?;
    self.reset_realm();
    self.nav_request_slot.clear();
    self.lifecycle.reset();
    if let Err(e) = self.target_session().send(method, json!({})).await {
      // No history entry in that direction: Playwright maps the
      // protocol's "Failed to go" to a null response without waiting
      // (`wkPage.ts::goBack`/`goForward`).
      if e.to_string().contains("Failed to go") {
        return Ok(None);
      }
      return Err(conn_err(e));
    }
    // A step onto an entry of the same document fires no load; Playwright
    // answers it with a null response once it commits.
    if self.wait_for_history_commit(lifecycle, timeout_ms).await? {
      return Ok(None);
    }
    Ok(self.await_nav_response(crate::network::NAV_REQUEST_GRACE).await)
  }

  /// [`Self::wait_for_lifecycle`] for a history step: also done when the
  /// main frame navigated within its document, which it reports as `true`.
  async fn wait_for_history_commit(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<bool> {
    let signals = Arc::clone(&self.lifecycle);
    let nav_slot = self.nav_request_slot.clone();
    let result = tokio::time::timeout_at(crate::backend::deadline_for(timeout_ms), async move {
      loop {
        let notified = signals.notify.notified();
        if signals.seen(lifecycle) {
          return Ok(false);
        }
        if signals.same_document.load(Ordering::SeqCst) && !signals.seen(NavLifecycle::Commit) {
          return Ok(true);
        }
        if let Some(err) = signals.provisional_failure(None) {
          return Err(FerriError::backend(format!("webkit navigate: {err}")));
        }
        if let Some((event_request_id, err)) = signals.failure()
          && let Some(req) = nav_slot.get_for(None)
          && event_request_id == req.id()
        {
          return Err(FerriError::backend(format!("webkit navigate: {err}")));
        }
        notified.await;
      }
    })
    .await;
    match result {
      Ok(inner) => inner,
      Err(_) => Err(FerriError::timeout(
        format!("waiting for navigation lifecycle {lifecycle:?}"),
        timeout_ms,
      )),
    }
  }

  /// # Errors
  ///
  /// Returns an error when resolving the main-world global with `Runtime.evaluate` or evaluating
  /// `location.href` with `Runtime.callFunctionOn` fails, or `FerriError::Evaluation` when it
  /// throws.
  pub async fn url(&self) -> Result<Option<String>> {
    Ok(self.eval_value("location.href").await?.as_str().map(String::from))
  }

  /// # Errors
  ///
  /// Returns an error when resolving the main-world global with `Runtime.evaluate` or evaluating
  /// `document.title` with `Runtime.callFunctionOn` fails, or `FerriError::Evaluation` when it
  /// throws.
  pub async fn title(&self) -> Result<Option<String>> {
    Ok(self.eval_value("document.title").await?.as_str().map(String::from))
  }
}
