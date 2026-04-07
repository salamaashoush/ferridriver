//! Video recording via CDP screencast + ffmpeg encoding.
//!
//! Architecture: CDP `Page.startScreencast` sends JPEG frames as events.
//! Frames are decoded and piped to an ffmpeg subprocess that encodes VP8/WebM.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::Page;

// ── VideoRecordingHandle: the public API ──

/// Handle for an active video recording session.
/// Owns the ffmpeg process and the frame pump task.
pub struct VideoRecordingHandle {
  ffmpeg_child: tokio::process::Child,
  pump_task: JoinHandle<()>,
  output_path: PathBuf,
}

/// Start recording: spawn ffmpeg, start CDP screencast, pump frames.
pub async fn start_recording(
  page: &Page,
  output_path: PathBuf,
  width: u32,
  height: u32,
  quality: u8,
) -> Result<VideoRecordingHandle, String> {
  // Ensure even dimensions (required for VP8).
  let w = width & !1;
  let h = height & !1;

  // 1. Spawn ffmpeg, ready to receive JPEG frames on stdin.
  let mut child = tokio::process::Command::new("ffmpeg")
    .args([
      "-f", "image2pipe",
      "-framerate", "25",
      "-i", "pipe:0",
      "-c:v", "libvpx",
      "-auto-alt-ref", "0",
      "-deadline", "realtime",
      "-b:v", "1M",
      "-vf", &format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2"),
      "-y",
    ])
    .arg(output_path.as_os_str())
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .map_err(|e| {
      if e.kind() == std::io::ErrorKind::NotFound {
        "Video recording requires ffmpeg. Install with: apt install ffmpeg / brew install ffmpeg".to_string()
      } else {
        format!("failed to spawn ffmpeg: {e}")
      }
    })?;

  // Take stdin — we'll pass it to the pump task.
  let stdin = child.stdin.take().ok_or("ffmpeg stdin not available")?;

  // 2. Start CDP screencast — returns a channel of decoded JPEG frames.
  let frame_rx = page.start_screencast(quality, w, h).await?;

  // 3. Spawn frame pump: reads JPEG frames from channel, writes to ffmpeg stdin.
  let pump_task = tokio::spawn(frame_pump(frame_rx, stdin));

  Ok(VideoRecordingHandle {
    ffmpeg_child: child,
    pump_task,
    output_path,
  })
}

impl VideoRecordingHandle {
  /// Stop recording: stop screencast, close ffmpeg, return video path.
  #[allow(unused_mut)]
  pub async fn stop(mut self, page: &Page) -> Result<PathBuf, String> {
    // 1. Stop CDP screencast (page session must still be alive).
    let _ = page.stop_screencast().await;

    // 2. Abort the frame pump (drops stdin → signals EOF to ffmpeg).
    self.pump_task.abort();
    let _ = self.pump_task.await;

    // 3. Wait for ffmpeg to finish encoding.
    let status = self.ffmpeg_child.wait().await.map_err(|e| format!("ffmpeg wait: {e}"))?;
    if !status.success() {
      tracing::debug!("ffmpeg exited with {status} (short test may produce too few frames)");
      // Remove incomplete file if ffmpeg failed (e.g., too few frames for encoding).
      let _ = std::fs::remove_file(&self.output_path);
    }

    Ok(self.output_path)
  }
}

/// Read JPEG frames from the channel and write to ffmpeg stdin.
async fn frame_pump(
  mut frame_rx: mpsc::UnboundedReceiver<Vec<u8>>,
  mut stdin: tokio::process::ChildStdin,
) {
  while let Some(frame) = frame_rx.recv().await {
    if stdin.write_all(&frame).await.is_err() {
      break;
    }
  }
  // Dropping stdin signals EOF to ffmpeg.
}
