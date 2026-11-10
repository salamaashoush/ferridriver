//! Chromium's own tab recorder, `Page.startScreenRecording` (Chrome M153
//! and later): frames go from the compositor to an encoder inside the
//! browser, which writes AV1 into an MP4 and hands it back as an IO stream.
//! Nothing crosses the wire while the page records, and stopping costs
//! milliseconds where the JPEG screencast and ffmpeg take seconds.
//!
//! The recorder runs on a CDP session of its own: Chromium refuses it and
//! the JPEG screencast (the trace film strip, a live view) on one session.
//! The MP4 exists only once the recording stops, and the recording goes
//! with the page's target, so every path that closes a page calls
//! [`ScreenRecorder::finish`] before it does.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, watch};

use super::transport::CdpTransport;
use crate::error::{FerriError, Result};

/// A finished recording: the MP4, or why it could not be kept.
pub type Recorded = std::result::Result<Arc<Vec<u8>>, String>;

#[derive(Debug, Clone, Copy)]
pub struct RecordingParams {
  pub max_width: u32,
  pub max_height: u32,
  pub frame_rate: u32,
}

/// The page's native recording, shared by every handle to the page.
#[derive(Clone, Default)]
pub struct ScreenRecorder {
  active: Arc<Mutex<Option<Active>>>,
}

struct Active {
  session: String,
  stream: String,
  done: watch::Sender<Option<Recorded>>,
}

/// How long a new page's target may take to accept a recording. Chromium
/// answers `Not attached to an active page` until the page's first frame
/// host is up, about 45 ms after the target appears.
const START_WINDOW: Duration = Duration::from_secs(2);

impl ScreenRecorder {
  /// Starts recording `target_id`, or answers `None` when this browser
  /// will not: a Chrome before M153 lacks the command, and a page records
  /// one recording at a time. The caller records from the screencast
  /// instead.
  pub async fn start<T: CdpTransport>(
    &self,
    transport: &T,
    target_id: &str,
    params: RecordingParams,
  ) -> Option<watch::Receiver<Option<Recorded>>> {
    let mut active = self.active.lock().await;
    if active.is_some() {
      return None;
    }
    let session = match transport
      .send_command(
        None,
        "Target.attachToTarget",
        &serde_json::json!({ "targetId": target_id, "flatten": true }),
      )
      .await
    {
      Ok(attached) => attached
        .get("sessionId")
        .and_then(serde_json::Value::as_str)?
        .to_owned(),
      Err(error) => {
        tracing::debug!(target: "ferridriver::video", "no session to record on: {error}");
        return None;
      },
    };
    let request = serde_json::json!({
      "maxWidth": params.max_width,
      "maxHeight": params.max_height,
      "frameRate": params.frame_rate,
    });
    let deadline = Instant::now() + START_WINDOW;
    let stream = loop {
      match transport
        .send_command(Some(&session), "Page.startScreenRecording", &request)
        .await
      {
        Ok(started) => {
          if let Some(stream) = started.get("stream").and_then(serde_json::Value::as_str) {
            break stream.to_owned();
          }
          detach(transport, &session).await;
          return None;
        },
        Err(FerriError::Protocol { message, .. })
          if message == "Not attached to an active page" && Instant::now() < deadline =>
        {
          tokio::time::sleep(Duration::from_millis(25)).await;
        },
        Err(error) => {
          tracing::debug!(target: "ferridriver::video", "recording from the screencast instead: {error}");
          detach(transport, &session).await;
          return None;
        },
      }
    };
    let (done, recorded) = watch::channel(None);
    *active = Some(Active { session, stream, done });
    Some(recorded)
  }

  /// Stops the recording, if one runs, and hands its MP4 to whoever
  /// started it. Answers at once when nothing records, so every close path
  /// can call it.
  pub async fn finish<T: CdpTransport>(&self, transport: &T) {
    let Some(active) = self.active.lock().await.take() else {
      return;
    };
    let recorded = stop(transport, &active)
      .await
      .map(Arc::new)
      .map_err(|error| format!("the recording was lost: {error}"));
    detach(transport, &active.session).await;
    let _ = active.done.send(Some(recorded));
  }
}

async fn stop<T: CdpTransport>(transport: &T, active: &Active) -> Result<Vec<u8>> {
  transport
    .send_command(
      Some(&active.session),
      "Page.stopScreenRecording",
      &super::super::EMPTY_PARAMS,
    )
    .await?;
  read_stream(transport, Some(&active.session), &active.stream).await
}

async fn detach<T: CdpTransport>(transport: &T, session: &str) {
  let _ = transport
    .send_command(
      None,
      "Target.detachFromTarget",
      &serde_json::json!({ "sessionId": session }),
    )
    .await;
}

/// Reads an IO stream to its end and closes it. Chromium answers `Read
/// failed` while a stream has nothing yet, as a recording's does until
/// its encoder flushes, so an empty read is tried again for a while.
pub(crate) async fn read_stream<T: CdpTransport>(
  transport: &T,
  session: Option<&str>,
  handle: &str,
) -> Result<Vec<u8>> {
  use base64::Engine as _;
  const EMPTY_READS: u32 = 100;
  let mut bytes = Vec::new();
  let mut empty = 0;
  loop {
    let chunk = match transport
      .send_command(session, "IO.read", &serde_json::json!({ "handle": handle }))
      .await
    {
      Ok(chunk) => chunk,
      Err(FerriError::Protocol { message, .. }) if message == "Read failed" && empty < EMPTY_READS => {
        empty += 1;
        tokio::time::sleep(Duration::from_millis(20)).await;
        continue;
      },
      Err(error) => return Err(error),
    };
    let data = chunk.get("data").and_then(serde_json::Value::as_str).unwrap_or("");
    if chunk
      .get("base64Encoded")
      .and_then(serde_json::Value::as_bool)
      .unwrap_or(false)
    {
      bytes.extend(
        base64::engine::general_purpose::STANDARD
          .decode(data)
          .map_err(|error| FerriError::protocol("IO.read", format!("invalid base64 chunk: {error}")))?,
      );
    } else {
      bytes.extend_from_slice(data.as_bytes());
    }
    if chunk.get("eof").and_then(serde_json::Value::as_bool).unwrap_or(false) {
      break;
    }
  }
  let _ = transport
    .send_command(session, "IO.close", &serde_json::json!({ "handle": handle }))
    .await;
  Ok(bytes)
}
