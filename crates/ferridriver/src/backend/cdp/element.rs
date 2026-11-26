//! `CdpElement` handle operations (DOM resolution, click, box model, files).

use super::*;

impl CdpElement {
  pub(crate) fn session_id(&self) -> Option<Arc<str>> {
    self.session_id.clone()
  }
  async fn cmd(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
    self
      .transport
      .send_command(self.session_id.as_deref(), method, &params)
      .await
  }

  async fn resolve_node_id_from_object(&self, object_id: &str) -> Result<i64> {
    let node_result = self
      .cmd("DOM.requestNode", serde_json::json!({"objectId": object_id}))
      .await?;
    let node_id = node_result
      .get("nodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.requestNode", "Could not resolve element nodeId"))?;
    if node_id == 0 {
      return Err(FerriError::protocol("DOM.requestNode", "Element not found"));
    }
    Ok(node_id)
  }

  async fn resolve_object_id_from_node(&self, node_id: i64) -> Result<Arc<str>> {
    let resolved = self
      .cmd("DOM.resolveNode", serde_json::json!({"nodeId": node_id}))
      .await?;
    resolved
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .map(Arc::from)
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", "Cannot resolve element"))
  }

  async fn node_id(&self) -> Result<i64> {
    let object_id = {
      let handles = self.handles.lock().await;
      if let Some(node_id) = handles.node_id {
        return Ok(node_id);
      }
      handles.object_id.clone()
    };

    let Some(object_id) = object_id else {
      return Err(FerriError::backend("Element handle has neither nodeId nor objectId"));
    };
    let node_id = self.resolve_node_id_from_object(&object_id).await?;
    let mut handles = self.handles.lock().await;
    handles.node_id = Some(node_id);
    Ok(node_id)
  }

  async fn object_id(&self) -> Result<Arc<str>> {
    let node_id = {
      let handles = self.handles.lock().await;
      if let Some(object_id) = &handles.object_id {
        return Ok(Arc::clone(object_id));
      }
      handles.node_id
    };

    let Some(node_id) = node_id else {
      return Err(FerriError::backend("Element handle has neither nodeId nor objectId"));
    };
    let object_id = self.resolve_object_id_from_node(node_id).await?;
    let mut handles = self.handles.lock().await;
    handles.object_id = Some(Arc::clone(&object_id));
    Ok(object_id)
  }

  /// Public accessor for the element's cached `RemoteObjectId`. Resolves
  /// and caches via `DOM.resolveNode` on first call. Used by
  /// [`crate::backend::element_handle_remote`] to hand an
  /// [`crate::js_handle::HandleRemote::Cdp`] payload back to
  /// [`crate::js_handle::JSHandle`] / [`crate::element_handle::ElementHandle`]
  /// at handle-materialization time.
  ///
  /// # Errors
  ///
  /// Returns an error if the element carries neither a cached `node_id`
  /// nor an `object_id` — should not happen for elements freshly
  /// returned from `find_element` / `evaluate_to_element`.
  pub async fn ensure_object_id(&self) -> Result<Arc<str>> {
    self.object_id().await
  }

  /// Get element center coordinates for clicking.
  async fn get_center(&self) -> Result<(f64, f64)> {
    let node_id = self.node_id().await?;
    let result = self
      .cmd("DOM.getBoxModel", serde_json::json!({"nodeId": node_id}))
      .await?;
    let content = result
      .get("model")
      .and_then(|m| m.get("content"))
      .and_then(|c| c.as_array())
      .ok_or_else(|| FerriError::protocol("DOM.getBoxModel", "No box model"))?;

    if content.len() < 8 {
      return Err(FerriError::protocol("DOM.getBoxModel", "Invalid box model"));
    }
    let x1 = content[0].as_f64().unwrap_or(0.0);
    let y1 = content[1].as_f64().unwrap_or(0.0);
    let x3 = content[4].as_f64().unwrap_or(0.0);
    let y3 = content[5].as_f64().unwrap_or(0.0);

    Ok((f64::midpoint(x1, x3), f64::midpoint(y1, y3)))
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.callFunctionOn`, the target has closed, or the command
  /// outlives its time budget. Also fails when the element cannot be resolved to an object, or the function
  /// throws.
  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>> {
    let object_id = self.object_id().await?;
    let result = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": &*object_id,
            "functionDeclaration": function,
            "returnByValue": true,
            // Allow `function` to be an async function or return a
            // Promise — CDP awaits the resolution before returning so
            // callers don't have to wrap the page-side helper in a
            // synthetic synchronous shell. Synchronous returns pay no
            // extra RTT; Chrome short-circuits when the result is not
            // a Promise.
            "awaitPromise": true,
        }),
      )
      .await?;
    thrown_in_page(&result)?;
    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  /// [`Self::call_js_fn_value`] in the frame's utility world, where the
  /// element is the browser's own and nothing the page put on it (a
  /// framework's own `value` property) is seen. The world is created by
  /// name, so every call reaches the same one.
  ///
  /// `known` is the utility world's execution context when the page
  /// already has it ([`CdpPage::known_utility_context`]): the three round
  /// trips that find the element's frame and ask for its world are then
  /// skipped. A context that has gone since (the document navigated) is
  /// refused by the browser, and the long way is taken.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.callFunctionOn`, `DOM.describeNode` or
  /// `Page.createIsolatedWorld`, the target has closed, or the command outlives its time budget. Also fails
  /// when the element cannot be resolved, has no document or frame, or the function throws.
  pub async fn call_js_fn_value_in_utility(
    &self,
    function: &str,
    known: Option<i64>,
  ) -> Result<Option<serde_json::Value>> {
    let object_id = self.object_id().await?;
    if let Some(context) = known
      && let Ok(adopted) = self.adopt_into(&object_id, context).await
    {
      return self.call_adopted(&adopted, function).await;
    }
    let root = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
          "objectId": &*object_id,
          "functionDeclaration": "function() { return (this.ownerDocument || this).documentElement; }",
        }),
      )
      .await?;
    thrown_in_page(&root)?;
    let root = root
      .pointer("/result/objectId")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::target_closed(Some("the element has no document".into())))?;
    let frame = self
      .cmd("DOM.describeNode", serde_json::json!({ "objectId": root }))
      .await?;
    let frame_id = frame
      .pointer("/node/frameId")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("DOM.describeNode", "the document has no frame"))?
      .to_string();
    let world = self
      .cmd(
        "Page.createIsolatedWorld",
        serde_json::json!({ "frameId": frame_id, "worldName": UTILITY_WORLD, "grantUniveralAccess": true }),
      )
      .await?;
    let context = world["executionContextId"]
      .as_i64()
      .ok_or_else(|| FerriError::protocol("Page.createIsolatedWorld", "missing execution context"))?;
    let adopted = self.adopt_into(&object_id, context).await?;
    self.call_adopted(&adopted, function).await
  }

  /// This element's handle in the world of `context`.
  async fn adopt_into(&self, object_id: &str, context: i64) -> Result<String> {
    let node = self
      .cmd("DOM.describeNode", serde_json::json!({ "objectId": object_id }))
      .await?;
    let backend_node = node
      .pointer("/node/backendNodeId")
      .and_then(serde_json::Value::as_i64)
      .ok_or_else(|| FerriError::protocol("DOM.describeNode", "missing backendNodeId"))?;
    let adopted = self
      .cmd(
        "DOM.resolveNode",
        serde_json::json!({ "backendNodeId": backend_node, "executionContextId": context }),
      )
      .await?;
    adopted
      .pointer("/object/objectId")
      .and_then(serde_json::Value::as_str)
      .map(str::to_string)
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", "the element is not in the utility world"))
  }

  /// Call `function` on an adopted handle and release it asynchronously so
  /// callers never block on a trailing `Runtime.releaseObject` round trip.
  async fn call_adopted(&self, adopted: &str, function: &str) -> Result<Option<serde_json::Value>> {
    let result = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
          "objectId": adopted,
          "functionDeclaration": function,
          "returnByValue": true,
          "awaitPromise": true,
        }),
      )
      .await;
    let transport = Arc::clone(&self.transport);
    let session = self.session_id.clone();
    let adopted_id = adopted.to_string();
    tokio::spawn(async move {
      let _ = transport
        .send(
          session.as_deref(),
          &ferridriver_cdp_proto::runtime::ReleaseObject { object_id: &adopted_id },
        )
        .await;
    });
    let result = result?;
    thrown_in_page(&result)?;
    Ok(result.get("result").and_then(|r| r.get("value")).cloned())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent` or `Runtime.callFunctionOn`, the target
  /// has closed, or the command outlives its time budget. Also fails when the element cannot be resolved or
  /// scrolled into view.
  pub async fn click(&self) -> Result<()> {
    // `Input.dispatchMouseEvent` uses top-level page coordinates. Walk
    // up the frame chain (`window.frameElement.getBoundingClientRect()`)
    // and accumulate per-iframe offsets so a button inside an iframe
    // lands at the right page-level coords. Playwright achieves this by
    // having a per-frame CDP session — we have a single session, so
    // we do the offset math in JS instead.
    let center = self
      .call_js_fn_value(
        "function() {
          this.scrollIntoViewIfNeeded();
          var r = this.getBoundingClientRect();
          var x = r.x + r.width / 2;
          var y = r.y + r.height / 2;
          var win = this.ownerDocument.defaultView;
          while (win && win !== win.parent && win.frameElement) {
            var fr = win.frameElement.getBoundingClientRect();
            x += fr.x;
            y += fr.y;
            win = win.parent;
          }
          return { x: x, y: y };
        }",
      )
      .await?;

    if let Some(c) = center {
      let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      if x == 0.0 && y == 0.0 {
        return self.call_js_fn("function() { this.click(); }").await;
      }
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      Ok(())
    } else {
      self.call_js_fn("function() { this.click(); }").await
    }
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.dispatchMouseEvent` or `Runtime.callFunctionOn`, the target
  /// has closed, or the command outlives its time budget. Also fails when the element cannot be resolved or
  /// scrolled into view.
  pub async fn dblclick(&self) -> Result<()> {
    let center = self.call_js_fn_value(
            "function() { this.scrollIntoViewIfNeeded(); var r = this.getBoundingClientRect(); return {x: r.x + r.width/2, y: r.y + r.height/2}; }"
        ).await?;

    if let Some(c) = center {
      let x = c.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      let y = c.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
      if x == 0.0 && y == 0.0 {
        return self
          .call_js_fn("function() { this.dispatchEvent(new MouseEvent('dblclick', {bubbles:true})); }")
          .await;
      }
      // First click (clickCount=1) fires 'click'
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}),
        )
        .await?;
      // Second click (clickCount=2) fires 'dblclick'
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 2}),
        )
        .await?;
      self
        .cmd(
          "Input.dispatchMouseEvent",
          serde_json::json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 2}),
        )
        .await?;
      Ok(())
    } else {
      self
        .call_js_fn("function() { this.dispatchEvent(new MouseEvent('dblclick', {bubbles:true})); }")
        .await
    }
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `DOM.scrollIntoViewIfNeeded`, `DOM.getBoxModel` or
  /// `Input.dispatchMouseEvent`, the target has closed, or the command outlives its time budget. Also fails
  /// when the element cannot be resolved.
  pub async fn hover(&self) -> Result<()> {
    self.scroll_into_view().await?;
    let (x, y) = self.get_center().await?;
    self
      .cmd(
        "Input.dispatchMouseEvent",
        serde_json::json!({"type": "mouseMoved", "x": x, "y": y}),
      )
      .await?;
    Ok(())
  }

  /// Set the file list on this `<input type=file>` element. Mirrors
  /// Playwright's `crPage.ts::setInputFilePaths`, which sends
  /// `DOM.setFileInputFiles` with the resolved handle's `objectId` — the
  /// element was resolved through the selector engine in the owning
  /// frame's context immediately before this call, so a stale node
  /// surfaces as a retryable protocol error and the locator funnel
  /// re-resolves instead of reusing a dead reference.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `DOM.setFileInputFiles`, the target has closed, or the command
  /// outlives its time budget. Also fails when the element cannot be resolved.
  pub async fn set_input_files(&self, paths: &[String]) -> Result<()> {
    let object_id = self.object_id().await?;
    self
      .cmd(
        "DOM.setFileInputFiles",
        serde_json::json!({"files": paths, "objectId": &*object_id}),
      )
      .await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Input.insertText`, the target has closed, or the command outlives
  /// its time budget. Also fails when clicking the element to focus it fails.
  pub async fn type_str(&self, text: &str) -> Result<()> {
    self.click().await?;
    self.cmd("Input.insertText", serde_json::json!({"text": text})).await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Runtime.callFunctionOn`, the target has closed, or the command
  /// outlives its time budget. Also fails when the element cannot be resolved.
  pub async fn call_js_fn(&self, function: &str) -> Result<()> {
    let object_id = self.object_id().await?;
    let result = self
      .cmd(
        "Runtime.callFunctionOn",
        serde_json::json!({
            "objectId": &*object_id,
            "functionDeclaration": function,
        }),
      )
      .await?;
    thrown_in_page(&result)
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `DOM.scrollIntoViewIfNeeded`, the target has closed, or the command
  /// outlives its time budget. Also fails when the element cannot be resolved.
  pub async fn scroll_into_view(&self) -> Result<()> {
    // Prefer `objectId` over `nodeId`. CDP's `DOM.scrollIntoViewIfNeeded`
    // accepts either, but the DOM agent's `nodeId` map is keyed on the
    // current document version returned by `DOM.getDocument`. Lazy
    // bootstrap (PERF_AUDIT §M.4) skips that call to save an RTT, so
    // `DOM.requestNode` reports `nodeId: 0` on a fresh page and the
    // caller sees a spurious "Element not found". Routing through
    // the Runtime `objectId` (already resolved by the page-side
    // selector engine) bypasses the DOM-agent state entirely.
    let object_id = self.object_id().await?;
    self
      .cmd(
        "DOM.scrollIntoViewIfNeeded",
        serde_json::json!({"objectId": &*object_id}),
      )
      .await?;
    Ok(())
  }
}
