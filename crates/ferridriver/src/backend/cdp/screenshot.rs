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
  let rendered = transport
    .send_command(
      session_id,
      "Runtime.evaluate",
      &json!({
        "expression": "new Promise(resolve => requestAnimationFrame(() => resolve(undefined)))",
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
