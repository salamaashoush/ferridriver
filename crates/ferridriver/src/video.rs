//! Video recording via CDP screencast + in-process ffmpeg encoding.
//!
//! Architecture: encoding runs on a blocking thread CONCURRENTLY with the test,
//! driven by a bounded channel from the CDP screencast pump. No subprocess spawning.
//!
//! **Eager** (`start_recording`): encoder thread runs during the test.
//! **Deferred** (`start_buffered_recording`): buffers frames, encodes only if needed.
//!
//! Shutdown is natural: stop_screencast -> pump sees channel close -> encoder drains
//! remaining frames -> finishes. No abort, no hang.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use crate::Page;

const FPS: u32 = 25;

pub use crate::ffmpeg::{video_content_type, video_extension};

// ── Eager recording: encode concurrently with the test ──

pub struct VideoRecordingHandle {
  /// Pump task: CDP screencast -> channel.
  pump_task: tokio::task::JoinHandle<()>,
  /// Encoder task: channel -> ffmpeg encode (blocking thread).
  encoder_task: tokio::task::JoinHandle<Result<(), String>>,
  output_path: PathBuf,
}

/// Start recording. Encoding runs concurrently on a blocking thread.
pub async fn start_recording(
  page: &Page,
  output_path: PathBuf,
  width: u32,
  height: u32,
  quality: u8,
) -> Result<VideoRecordingHandle, String> {
  let w = width & !1;
  let h = height & !1;

  // Bounded channel: CDP pump -> encoder. Backpressure prevents unbounded buffering.
  let (frame_tx, frame_rx) = mpsc::channel::<(Vec<u8>, f64)>(64);

  // Start CDP screencast.
  let cdp_rx = page.start_screencast(quality, w, h).await?;

  // Pump task: forwards CDP frames (with Chrome timestamps) to encoder channel.
  let pump_task = tokio::spawn(async move {
    let mut rx = cdp_rx;
    while let Some((jpeg, ts)) = rx.recv().await {
      if frame_tx.send((jpeg, ts)).await.is_err() {
        break; // Encoder dropped, stop pumping.
      }
    }
    // frame_tx dropped here -> encoder sees channel close -> drains and finishes.
  });

  // Encoder runs on blocking thread, driven by channel.
  let path = output_path.clone();
  let encoder_task = tokio::task::spawn_blocking(move || crate::ffmpeg::encode_stream(frame_rx, &path, w, h, FPS));

  Ok(VideoRecordingHandle {
    pump_task,
    encoder_task,
    output_path,
  })
}

impl VideoRecordingHandle {
  /// Stop recording: stop screencast, wait for encoder to finish.
  pub async fn stop(self, page: &Page) -> Result<PathBuf, String> {
    // Stop CDP screencast. This makes Chrome stop sending frames.
    let _ = page.stop_screencast().await;

    // Pump task will exit because cdp_rx closes (no more screencastFrame events).
    // When pump exits, frame_tx drops, encoder sees channel close and drains.
    // Abort pump in case it's stuck waiting on a frame that'll never come.
    self.pump_task.abort();

    // Wait for encoder to finish encoding remaining frames + trailing pad.
    self.encoder_task.await.map_err(|e| format!("encoder join: {e}"))??;

    Ok(self.output_path)
  }
}

// ── Deferred (buffered) recording: zero encoding cost for passing tests ──

pub struct BufferedRecordingHandle {
  frames: Arc<Mutex<Vec<(Vec<u8>, f64)>>>,
  pump_task: tokio::task::JoinHandle<()>,
  width: u32,
  height: u32,
}

/// Start buffered recording: buffer frames in memory, no encoding until requested.
pub async fn start_buffered_recording(
  page: &Page,
  width: u32,
  height: u32,
  quality: u8,
) -> Result<BufferedRecordingHandle, String> {
  let w = width & !1;
  let h = height & !1;

  let cdp_rx = page.start_screencast(quality, w, h).await?;
  let frames: Arc<Mutex<Vec<(Vec<u8>, f64)>>> = Arc::new(Mutex::new(Vec::with_capacity(64)));

  let frames_clone = Arc::clone(&frames);
  let pump_task = tokio::spawn(async move {
    let mut rx = cdp_rx;
    while let Some((jpeg, ts)) = rx.recv().await {
      frames_clone.lock().await.push((jpeg, ts));
    }
  });

  Ok(BufferedRecordingHandle {
    frames,
    pump_task,
    width: w,
    height: h,
  })
}

impl BufferedRecordingHandle {
  /// Stop capturing and encode buffered frames to video.
  pub async fn encode(self, page: &Page, output_path: PathBuf) -> Result<PathBuf, String> {
    let _ = page.stop_screencast().await;
    self.pump_task.abort();
    let _ = self.pump_task.await;

    let frames = self.frames.lock().await;
    if frames.is_empty() {
      return Err("no frames captured".into());
    }

    let w = self.width;
    let h = self.height;
    let frames_owned: Vec<(Vec<u8>, f64)> = frames.clone();
    drop(frames);

    let path = output_path.clone();
    tokio::task::spawn_blocking(move || crate::ffmpeg::encode_frames(&frames_owned, &path, w, h, FPS))
      .await
      .map_err(|e| format!("encode join: {e}"))??;

    Ok(output_path)
  }

  /// Stop capturing and discard frames. Zero encoding cost.
  pub async fn discard(self, page: &Page) {
    let _ = page.stop_screencast().await;
    self.pump_task.abort();
    let _ = self.pump_task.await;
  }
}
