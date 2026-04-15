//! `BiDi` element -- implements element interactions via `SharedReferences`.
//!
//! Uses `script.callFunction` with element as argument for JS operations,
//! and `input.performActions` for user input simulation.

use std::sync::Arc;

use base64::Engine;
use serde_json::json;

use super::input;
use super::session::BidiSession;
use super::types::EvaluateResult;
use crate::backend::ImageFormat;

/// Element handle for the `BiDi` backend.
pub struct BidiElement {
  pub(crate) session: Arc<BidiSession>,
  pub(crate) context_id: Arc<str>,
  pub(crate) shared_id: String,
}

impl BidiElement {
  pub(crate) fn new(session: Arc<BidiSession>, context_id: Arc<str>, shared_id: String) -> Self {
    Self {
      session,
      context_id,
      shared_id,
    }
  }

  /// Call a JS function with this element.
  /// The element is passed both as `this` (for `function() { this.value }` style)
  /// and as the first argument (for `(el) => el.value` style).
  /// This matches CDP's `callFunctionOn` which binds `this` to the target object.
  async fn call_fn(&self, func: &str) -> Result<serde_json::Value, String> {
    self
      .session
      .transport
      .send_command(
        "script.callFunction",
        json!({
          "functionDeclaration": func,
          "target": {"context": &*self.context_id},
          "this": {"type": "sharedReference", "sharedId": self.shared_id},
          "arguments": [{"type": "sharedReference", "sharedId": self.shared_id}],
          "awaitPromise": true,
          "resultOwnership": "none"
        }),
      )
      .await
  }

  /// Call a JS function and parse the evaluate result to a JSON value.
  async fn call_fn_value(&self, func: &str) -> Result<Option<serde_json::Value>, String> {
    let result = self.call_fn(func).await?;
    let eval_result: EvaluateResult =
      serde_json::from_value(result).map_err(|e| format!("BiDi element call_fn parse: {e}"))?;

    match eval_result {
      EvaluateResult::Success { result } => Ok(result.to_json()),
      EvaluateResult::Exception { exception_details } => {
        Err(format!("JS error on element: {}", exception_details.text))
      },
    }
  }

  /// Get the element's bounding box.
  async fn bounding_box(&self) -> Result<(f64, f64, f64, f64), String> {
    let result = self
      .call_fn_value(
        "(el) => { const r = el.getBoundingClientRect(); return {x: r.x, y: r.y, w: r.width, h: r.height}; }",
      )
      .await?
      .ok_or("Element bounding box returned null")?;

    tracing::debug!(target: "ferridriver::bidi", bbox_json = %result, "BiDi bounding box result");

    let x = result.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let y = result.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let w = result.get("w").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let h = result.get("h").and_then(serde_json::Value::as_f64).unwrap_or(0.0);

    Ok((x, y, w, h))
  }

  pub async fn click(&self) -> Result<(), String> {
    self.scroll_into_view().await?;
    let (x, y, w, h) = self.bounding_box().await?;
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    tracing::debug!(target: "ferridriver::bidi", x, y, w, h, cx, cy, shared_id = %self.shared_id, "BiDi element click");
    self
      .session
      .transport
      .send_command("input.performActions", input::click(&self.context_id, cx, cy))
      .await?;
    Ok(())
  }

  pub async fn dblclick(&self) -> Result<(), String> {
    self.scroll_into_view().await?;
    let (x, y, w, h) = self.bounding_box().await?;
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    self
      .session
      .transport
      .send_command(
        "input.performActions",
        input::click_button(&self.context_id, cx, cy, 0, 2),
      )
      .await?;
    Ok(())
  }

  pub async fn hover(&self) -> Result<(), String> {
    self.scroll_into_view().await?;
    let (x, y, w, h) = self.bounding_box().await?;
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    self
      .session
      .transport
      .send_command("input.performActions", input::pointer_move(&self.context_id, cx, cy))
      .await?;
    Ok(())
  }

  pub async fn type_str(&self, text: &str) -> Result<(), String> {
    // Click to focus first
    self.click().await?;
    self
      .session
      .transport
      .send_command("input.performActions", input::type_text(&self.context_id, text))
      .await?;
    Ok(())
  }

  pub async fn call_js_fn(&self, function: &str) -> Result<(), String> {
    let result = self.call_fn(function).await?;
    let eval_result: EvaluateResult =
      serde_json::from_value(result).map_err(|e| format!("BiDi element call_js_fn parse: {e}"))?;

    match eval_result {
      EvaluateResult::Success { .. } => Ok(()),
      EvaluateResult::Exception { exception_details } => {
        Err(format!("JS error on element: {}", exception_details.text))
      },
    }
  }

  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<serde_json::Value>, String> {
    self.call_fn_value(function).await
  }

  pub async fn scroll_into_view(&self) -> Result<(), String> {
    let _ = self
      .call_fn("(el) => el.scrollIntoView({block: 'center', inline: 'center'})")
      .await;
    Ok(())
  }

  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>, String> {
    let format_type = match format {
      ImageFormat::Png => "image/png",
      ImageFormat::Jpeg => "image/jpeg",
      ImageFormat::Webp => "image/webp",
    };

    let result = self
      .session
      .transport
      .send_command(
        "browsingContext.captureScreenshot",
        json!({
          "context": &*self.context_id,
          "format": {"type": format_type},
          "clip": {"type": "element", "element": {"sharedId": self.shared_id}}
        }),
      )
      .await?;

    let data_str = result
      .get("data")
      .and_then(|v| v.as_str())
      .ok_or("Element screenshot: missing data")?;
    base64::engine::general_purpose::STANDARD
      .decode(data_str)
      .map_err(|e| format!("Element screenshot base64 decode: {e}"))
  }
}
