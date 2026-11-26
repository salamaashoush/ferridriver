use base64::Engine as _;
use serde_json::Value;

use super::transport::CdpTransport;
use crate::error::{FerriError, Result};

pub(super) async fn capture(transport: &CdpTransport, session_id: Option<&str>, params: Value) -> Result<Vec<u8>> {
  // Layout metrics can be valid before Chromium has a captureable surface.
  // Synchronize with rendering so the first capture cannot sample that gap.
  // The gap closes with the document's first frame, so only the first
  // capture of a document waits: the mark lives on the document's global
  // and goes away with it. Waiting every time costs each screenshot a
  // whole frame on top of the one `Page.captureScreenshot` draws itself.
  let rendered = transport
    .send(
      session_id,
      &ferridriver_cdp_proto::runtime::Evaluate {
        expression: "(() => { const k = Symbol.for('ferridriver.painted'); if (globalThis[k]) return undefined; \
                     return new Promise(resolve => requestAnimationFrame(() => { globalThis[k] = true; resolve(undefined); })); })()",
        context_id: None,
        return_by_value: Some(true),
        await_promise: Some(true),
        user_gesture: None,
        silent: None,
      },
    )
    .await?;
  if let Some(exception) = rendered.exception_details {
    let msg = if exception.text.is_empty() {
      "Screenshot rendering failed"
    } else {
      exception.text.as_str()
    };
    return Err(FerriError::backend(msg));
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
