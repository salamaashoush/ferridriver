use base64::Engine as _;
use serde_json::{Value, json};

use super::transport::CdpTransport;
use crate::error::{FerriError, Result};

pub(super) async fn capture<T: CdpTransport>(
  transport: &T,
  session_id: Option<&str>,
  params: Value,
) -> Result<Vec<u8>> {
  // Layout metrics can be valid before Chromium has a captureable surface.
  // Synchronize with rendering so the first capture cannot sample that gap.
  // The gap closes with the document's first frame, so only the first
  // capture of a document waits: the mark lives on the document's global
  // and goes away with it. Waiting every time costs each screenshot a
  // whole frame on top of the one `Page.captureScreenshot` draws itself.
  let rendered = transport
    .send_command(
      session_id,
      "Runtime.evaluate",
      &json!({
        "expression": "(() => { const k = Symbol.for('ferridriver.painted'); if (globalThis[k]) return undefined; \
                       return new Promise(resolve => requestAnimationFrame(() => { globalThis[k] = true; resolve(undefined); })); })()",
        "awaitPromise": true,
        "returnByValue": true,
      }),
    )
    .await?;
  if let Some(exception) = rendered.get("exceptionDetails") {
    return Err(FerriError::backend(
      exception
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("Screenshot rendering failed"),
    ));
  }
  let result = transport
    .send_command(session_id, "Page.captureScreenshot", &params)
    .await?;
  let data = result
    .get("data")
    .and_then(Value::as_str)
    .ok_or_else(|| FerriError::backend("No screenshot data"))?;
  base64::engine::general_purpose::STANDARD
    .decode(data)
    .map_err(|error| FerriError::Backend(format!("Decode screenshot: {error}")))
}
