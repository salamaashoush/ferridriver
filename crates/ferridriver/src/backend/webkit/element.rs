//! Playwright `WebKit` element handle — DOM-node operations addressed
//! by a `Runtime.RemoteObject.objectId` on the page's target session.

use base64::Engine as _;
use serde_json::{Value, json};

use super::connection::{ConnectionError, Session};
use super::protocol;
use crate::backend::ImageFormat;
use crate::error::{FerriError, Result};

/// Element handle for the PW `WebKit` backend.
#[derive(Clone)]
pub struct WebKitElement {
  target: Session,
  object_id: String,
}

impl WebKitElement {
  #[must_use]
  pub fn new(target: Session, object_id: String) -> Self {
    Self { target, object_id }
  }

  /// The backing `Runtime.RemoteObject.objectId`. Used by
  /// `element_handle_remote` when minting a `HandleRemote::WebKit`.
  #[must_use]
  pub fn object_id(&self) -> &str {
    &self.object_id
  }

  /// `Runtime.callFunctionOn` with this element bound as `this` and
  /// passed as the first argument. `return_by_value` controls whether
  /// the reply inlines the value.
  async fn call_fn(&self, function_declaration: &str, return_by_value: bool) -> Result<Value> {
    let resp = self
      .target
      .send(
        protocol::RUNTIME_CALL_FUNCTION_ON,
        json!({
          "objectId": self.object_id,
          "functionDeclaration": function_declaration,
          "arguments": [{ "objectId": self.object_id }],
          "returnByValue": return_by_value,
          "awaitPromise": true,
        }),
      )
      .await
      .map_err(map_err)?;
    if resp.get("wasThrown").and_then(Value::as_bool).unwrap_or(false) {
      let msg = resp
        .get("result")
        .and_then(|r| r.get("description").or_else(|| r.get("value")))
        .and_then(Value::as_str)
        .unwrap_or("element function threw")
        .to_string();
      return Err(FerriError::evaluation(msg));
    }
    Ok(resp)
  }

  /// Run `function_declaration` and return the inlined JSON value.
  async fn call_fn_value(&self, function_declaration: &str) -> Result<Option<Value>> {
    let resp = self.call_fn(function_declaration, true).await?;
    Ok(resp.get("result").and_then(|r| r.get("value")).cloned())
  }

  /// Element center in viewport coordinates after scrolling into view.
  async fn center(&self) -> Result<(f64, f64)> {
    let v = self
      .call_fn_value(
        "function(){this.scrollIntoView({block:'center',inline:'center'});\
         const r=this.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};}",
      )
      .await?
      .ok_or_else(|| FerriError::backend("webkit: element bounding box null"))?;
    Ok((
      v.get("x").and_then(Value::as_f64).unwrap_or(0.0),
      v.get("y").and_then(Value::as_f64).unwrap_or(0.0),
    ))
  }

  pub async fn click(&self) -> Result<()> {
    self.call_fn("function(){this.click();}", true).await?;
    Ok(())
  }

  pub async fn dblclick(&self) -> Result<()> {
    self
      .call_fn(
        "function(){this.dispatchEvent(new MouseEvent('dblclick',{bubbles:true,cancelable:true}));}",
        true,
      )
      .await?;
    Ok(())
  }

  pub async fn hover(&self) -> Result<()> {
    let (x, y) = self.center().await?;
    self
      .call_fn(
        &format!(
          "function(){{for(const t of ['mouseover','mouseenter','mousemove'])\
           this.dispatchEvent(new MouseEvent(t,{{bubbles:true,clientX:{x},clientY:{y}}}));}}"
        ),
        true,
      )
      .await?;
    Ok(())
  }

  pub async fn type_str(&self, text: &str) -> Result<()> {
    let escaped = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    self
      .call_fn(
        &format!(
          "function(){{this.focus();this.value=(this.value||'')+{escaped};\
           this.dispatchEvent(new Event('input',{{bubbles:true}}));\
           this.dispatchEvent(new Event('change',{{bubbles:true}}));}}"
        ),
        true,
      )
      .await?;
    Ok(())
  }

  pub async fn call_js_fn(&self, function: &str) -> Result<()> {
    self.call_fn(function, true).await?;
    Ok(())
  }

  pub async fn call_js_fn_value(&self, function: &str) -> Result<Option<Value>> {
    self.call_fn_value(function).await
  }

  pub async fn scroll_into_view(&self) -> Result<()> {
    self
      .call_fn(
        "function(){this.scrollIntoView({block:'center',inline:'center'});}",
        true,
      )
      .await?;
    Ok(())
  }

  pub async fn screenshot(&self, format: ImageFormat) -> Result<Vec<u8>> {
    let rect = self
      .call_fn_value(
        "function(){this.scrollIntoView({block:'center'});\
         const r=this.getBoundingClientRect();return {x:r.x,y:r.y,width:r.width,height:r.height};}",
      )
      .await?
      .ok_or_else(|| FerriError::backend("webkit: element rect null"))?;
    let resp = self
      .target
      .send(
        "Page.snapshotRect",
        json!({
          "x": rect.get("x").and_then(Value::as_f64).unwrap_or(0.0),
          "y": rect.get("y").and_then(Value::as_f64).unwrap_or(0.0),
          "width": rect.get("width").and_then(Value::as_f64).unwrap_or(0.0),
          "height": rect.get("height").and_then(Value::as_f64).unwrap_or(0.0),
          "coordinateSystem": "Viewport",
        }),
      )
      .await
      .map_err(map_err)?;
    let data_url = resp.get("dataURL").and_then(Value::as_str).unwrap_or_default();
    let b64 = data_url.split_once(',').map_or(data_url, |(_, d)| d);
    let png = base64::engine::general_purpose::STANDARD
      .decode(b64)
      .map_err(|e| FerriError::backend(format!("element screenshot base64: {e}")))?;
    match format {
      ImageFormat::Png => Ok(png),
      // `Page.snapshotRect` only produces PNG — transcode like the
      // page-screenshot path (Playwright transcodes client-side too,
      // jpeg-js quality default 80).
      ImageFormat::Jpeg => {
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
          .map_err(|e| FerriError::backend(format!("element screenshot png decode: {e}")))?;
        let mut out = std::io::Cursor::new(Vec::new());
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
        img
          .to_rgb8()
          .write_with_encoder(encoder)
          .map_err(|e| FerriError::backend(format!("element screenshot jpeg encode: {e}")))?;
        Ok(out.into_inner())
      },
      ImageFormat::Webp => Err(FerriError::unsupported(
        "screenshot type 'webp' is not supported on the WebKit backend (Page.snapshotRect produces PNG; Playwright supports webp on Chromium only)",
      )),
    }
  }

  /// Release the backing remote object (`Runtime.releaseObject`).
  pub async fn release(&self) -> Result<()> {
    let _ = self
      .target
      .send(protocol::RUNTIME_RELEASE_OBJECT, json!({ "objectId": self.object_id }))
      .await;
    Ok(())
  }
}

fn map_err(e: ConnectionError) -> FerriError {
  e.into()
}
