//! Video recording via the page's shared screencast + ffmpeg encoding.
//!
//! Architecture: encoding runs on a blocking thread CONCURRENTLY with the test,
//! driven by a bounded channel from a pump subscribed to the page's
//! screencast hub (shared with the trace film strip, so both can record).
//!
//! **Eager** (`start_recording`): encoder thread runs during the test.
//! **Deferred** (`start_buffered_recording`): buffers frames, encodes only if needed.
//!
//! The video's timeline is wall time: t=0 is the moment recording started
//! and the last frame is held until the moment it stopped, so a position
//! in the video maps linearly onto time in the test.
//!
//! Shutdown is natural: unsubscribing delivers the frames the screencast
//! already emitted, the pump sees the channel close and hands the stop
//! time to the encoder, which drains and finishes. No abort, no hang.
//!
//! # Playwright-facing `Video` handle (§2.14)
//!
//! [`Video`] is the user-facing class matching Playwright's
//! `/tmp/playwright/packages/playwright-core/src/client/video.ts` — sync
//! construction, async `path()` / `save_as()` / `delete()` that all
//! await the page-close-triggered recording finalise. Mirrors
//! `types.d.ts:21621` byte-for-byte:
//!
//! ```text
//! interface Video {
//!   delete(): Promise<void>;
//!   path(): Promise<string>;
//!   saveAs(path: string): Promise<void>;
//! }
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, watch};

use crate::Page;
use crate::backend::screencast::{ScreencastFrame, ScreencastParams, ScreencastSubscription};
use crate::error::FerriError;
use crate::ffmpeg::EncoderInput;

const FPS: u32 = 25;

pub use crate::ffmpeg::{video_content_type, video_extension};

/// Seconds from the recording's start to when `frame` reached us. A frame
/// replayed from before the start (the page's current picture) lands at 0.
fn seconds_since(started: Instant, frame: &ScreencastFrame) -> f64 {
  frame.received_at.saturating_duration_since(started).as_secs_f64()
}

async fn subscribe(
  page: &Page,
  width: u32,
  height: u32,
  quality: u8,
) -> crate::error::Result<(ScreencastSubscription, mpsc::UnboundedReceiver<ScreencastFrame>)> {
  page
    .inner()
    .subscribe_screencast(ScreencastParams::new(quality, width, height))
    .await
}

// ── Eager recording: encode concurrently with the test ──

pub struct VideoRecordingHandle {
  subscription: ScreencastSubscription,
  started: Instant,
  /// Pump task: screencast -> encoder channel. Returns the channel so
  /// `stop` can send the stop time after the last frame.
  pump_task: tokio::task::JoinHandle<mpsc::Sender<EncoderInput>>,
  /// Encoder task: channel -> ffmpeg encode (blocking thread).
  encoder_task: tokio::task::JoinHandle<crate::error::Result<()>>,
  output_path: PathBuf,
}

/// Start recording. Encoding runs concurrently on a blocking thread.
///
/// # Errors
///
/// Returns an error if the page's screencast cannot be started.
pub async fn start_recording(
  page: &Page,
  output_path: PathBuf,
  width: u32,
  height: u32,
  quality: u8,
) -> crate::error::Result<VideoRecordingHandle> {
  let w = width & !1;
  let h = height & !1;
  let started = Instant::now();

  // Bounded channel: pump -> encoder. Backpressure prevents unbounded buffering.
  let (frame_tx, frame_rx) = mpsc::channel::<EncoderInput>(64);
  let (subscription, mut frames) = subscribe(page, w, h, quality).await?;

  let pump_task = tokio::spawn(async move {
    while let Some(frame) = frames.recv().await {
      let at = seconds_since(started, &frame);
      if frame_tx
        .send(EncoderInput::Frame { jpeg: frame.data, at })
        .await
        .is_err()
      {
        break; // Encoder dropped, stop pumping.
      }
    }
    frame_tx
  });

  let path = output_path.clone();
  let encoder_task = tokio::task::spawn_blocking(move || crate::ffmpeg::encode_stream(frame_rx, &path, w, h, FPS));

  Ok(VideoRecordingHandle {
    subscription,
    started,
    pump_task,
    encoder_task,
    output_path,
  })
}

impl VideoRecordingHandle {
  /// Stop recording and wait for the encoder to finish. The video ends
  /// now, however long ago the page last changed.
  ///
  /// # Errors
  ///
  /// Returns an error if the encoder fails or the join handle panics.
  pub async fn stop(self) -> crate::error::Result<PathBuf> {
    let end = self.started.elapsed().as_secs_f64();
    self.subscription.stop().await;
    let frame_tx = self
      .pump_task
      .await
      .map_err(|e| FerriError::Backend(format!("video pump join: {e}")))?;
    let _ = frame_tx.send(EncoderInput::End { at: end }).await;
    drop(frame_tx);
    self
      .encoder_task
      .await
      .map_err(|e| FerriError::Backend(format!("encoder join: {e}")))??;

    tracing::debug!(target: "ferridriver::video", "recorded {end:.3}s into {}", self.output_path.display());
    Ok(self.output_path)
  }
}

// ── Deferred (buffered) recording: zero encoding cost for passing tests ──

/// Buffered `(jpeg_bytes, seconds_since_start)` frames.
type FrameBuffer = Vec<(Vec<u8>, f64)>;

pub struct BufferedRecordingHandle {
  subscription: ScreencastSubscription,
  started: Instant,
  pump_task: tokio::task::JoinHandle<FrameBuffer>,
  width: u32,
  height: u32,
}

/// Start buffered recording: buffer frames in memory, no encoding until requested.
///
/// # Errors
///
/// Returns an error if the page's screencast cannot be started.
pub async fn start_buffered_recording(
  page: &Page,
  width: u32,
  height: u32,
  quality: u8,
) -> crate::error::Result<BufferedRecordingHandle> {
  let w = width & !1;
  let h = height & !1;
  let started = Instant::now();
  let (subscription, mut frames) = subscribe(page, w, h, quality).await?;

  let pump_task = tokio::spawn(async move {
    let mut buffer = FrameBuffer::with_capacity(64);
    while let Some(frame) = frames.recv().await {
      let at = seconds_since(started, &frame);
      buffer.push((frame.data, at));
    }
    buffer
  });

  Ok(BufferedRecordingHandle {
    subscription,
    started,
    pump_task,
    width: w,
    height: h,
  })
}

impl BufferedRecordingHandle {
  /// Stop capturing and encode buffered frames to video.
  ///
  /// # Errors
  ///
  /// Returns an error if no frames were captured, encoding fails, or the join handle panics.
  pub async fn encode(self, output_path: PathBuf) -> crate::error::Result<PathBuf> {
    let end = self.started.elapsed().as_secs_f64();
    self.subscription.stop().await;
    let frames = self
      .pump_task
      .await
      .map_err(|e| FerriError::backend(format!("video pump join: {e}")))?;
    if frames.is_empty() {
      return Err(FerriError::backend("no frames captured"));
    }

    let w = self.width;
    let h = self.height;
    let path = output_path.clone();
    tokio::task::spawn_blocking(move || crate::ffmpeg::encode_frames(frames, end, &path, w, h, FPS))
      .await
      .map_err(|e| FerriError::backend(format!("encode join: {e}")))??;

    Ok(output_path)
  }

  /// Stop capturing and discard frames. Zero encoding cost.
  pub async fn discard(self) {
    self.subscription.stop().await;
    let _ = self.pump_task.await;
  }
}

// ── Public Video handle (§2.14) ─────────────────────────────────────────────

/// Terminal state of a recording: either the finalised file path, or a
/// typed error explaining why finalisation failed (backend does not
/// support recording, encoder crashed, page never closed, …).
type FinalPath = std::result::Result<PathBuf, FerriError>;

/// Live [`Video`] handle returned by [`crate::Page::video`]. Matches
/// Playwright's `page.video(): null | Video` (types.d.ts:4756) and the
/// `Video` interface at types.d.ts:21621.
///
/// All three accessor methods (`path`, `save_as`, `delete`) block on a
/// `tokio::sync::watch` channel until the owning page closes and the
/// encoder thread finishes — matches Playwright's contract that "the
/// video is guaranteed to be written to the filesystem upon closing the
/// browser context."
#[derive(Clone)]
pub struct Video {
  state: Arc<VideoState>,
}

struct VideoState {
  /// Receiver half of the watch channel. `None` while recording is in
  /// progress; `Some(Ok(path))` once the encoder finishes successfully;
  /// `Some(Err(reason))` if encoding or start-up failed. Cloneable —
  /// every Video clone shares the same terminal-state channel.
  path_rx: watch::Receiver<Option<FinalPath>>,
}

impl Video {
  /// Construct a new live [`Video`] handle + its paired
  /// [`VideoSink`]. The sink is the write-side handle the recording
  /// runtime uses to announce the terminal state; callers receive the
  /// read-only [`Video`] and dispatch the sink into whatever task
  /// actually awaits the page close / encoder completion.
  #[must_use]
  pub fn new() -> (Self, VideoSink) {
    let (tx, rx) = watch::channel(None);
    let video = Self {
      state: Arc::new(VideoState { path_rx: rx }),
    };
    let sink = VideoSink { tx };
    (video, sink)
  }

  /// Playwright: `video.path(): Promise<string>`. Resolves to the
  /// recorded file path once the owning page closes and the encoder
  /// finishes. Returns a typed [`FerriError::Unsupported`] (or a
  /// pass-through of the encoder's own error text) when recording
  /// couldn't be produced — matches Playwright's behaviour of
  /// surfacing the root cause rather than silently returning an empty
  /// string.
  ///
  /// # Errors
  ///
  /// * If the terminal state carries an error from the encoder / a
  ///   backend that cannot record, the error is returned verbatim as
  ///   [`FerriError::Unsupported`] (encoder path) or the raw reason
  ///   string wrapped in [`FerriError::Backend`].
  /// * If the owning page is dropped without ever finishing the
  ///   recording, returns [`FerriError::target_closed`].
  pub async fn path(&self) -> crate::error::Result<PathBuf> {
    self.await_terminal_state().await?
  }

  /// Playwright: `video.saveAs(path): Promise<void>`. Blocks until the
  /// recording is finalised, then `std::fs::copy`'s the file to
  /// `dest`. Safe to call during or after recording — matches
  /// Playwright's "safe to call this method while the video is still
  /// in progress" contract (types.d.ts:21634).
  ///
  /// # Errors
  ///
  /// * Propagates any finalise-time error (same shape as
  ///   [`Self::path`]).
  /// * Wraps I/O errors from [`std::fs::copy`] in
  ///   [`FerriError::Backend`].
  pub async fn save_as(&self, dest: impl AsRef<Path>) -> crate::error::Result<()> {
    let source = self.path().await?;
    let dest = dest.as_ref().to_path_buf();
    if let Some(parent) = dest.parent()
      && !parent.as_os_str().is_empty()
    {
      std::fs::create_dir_all(parent)
        .map_err(|e| FerriError::Backend(format!("video.saveAs create parent {}: {e}", parent.display())))?;
    }
    tokio::task::spawn_blocking(move || std::fs::copy(&source, &dest).map(|_| ()))
      .await
      .map_err(|e| FerriError::Backend(format!("video.saveAs join: {e}")))?
      .map_err(|e| FerriError::Backend(format!("video.saveAs copy: {e}")))
  }

  /// Playwright: `video.delete(): Promise<void>`. Blocks until the
  /// recording is finalised, then removes the file. If finalisation
  /// failed (e.g. `WebKit`), deletion is a no-op.
  ///
  /// # Errors
  ///
  /// Wraps I/O errors from [`std::fs::remove_file`] in
  /// [`FerriError::Backend`]; silences `NotFound` (idempotent delete).
  pub async fn delete(&self) -> crate::error::Result<()> {
    let Ok(path) = self.path().await else {
      // Finalisation failed; nothing to delete.
      return Ok(());
    };
    tokio::task::spawn_blocking(move || match std::fs::remove_file(&path) {
      Ok(()) => Ok(()),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(e) => Err(format!("video.delete remove {}: {e}", path.display())),
    })
    .await
    .map_err(|e| FerriError::Backend(format!("video.delete join: {e}")))?
    .map_err(FerriError::Backend)
  }

  async fn await_terminal_state(&self) -> crate::error::Result<FinalPath> {
    let mut rx = self.state.path_rx.clone();
    loop {
      if let Some(val) = rx.borrow_and_update().clone() {
        return Ok(val);
      }
      if rx.changed().await.is_err() {
        return Err(FerriError::target_closed(Some(
          "video recording was cancelled before finalisation".into(),
        )));
      }
    }
  }
}

/// Write-side handle for a [`Video`] — used exclusively by the
/// recording runtime (see `state::register_opened_page` wiring) to
/// announce the terminal state. Cheap to move; drop sets the channel
/// to `Err("dropped before completion")` automatically via the
/// `tokio::sync::watch` Sender being dropped.
pub struct VideoSink {
  tx: watch::Sender<Option<FinalPath>>,
}

impl VideoSink {
  /// Publish the successful finalise path. Subsequent callers of
  /// [`Video::path`] / [`Video::save_as`] / [`Video::delete`] observe
  /// the path and proceed. Uses `send_replace` so listeners that
  /// subscribe before the first publish see the terminal value on
  /// their next poll — matches the §2.12 pattern for one-shot
  /// terminal-state transitions on handles with lazy subscribers.
  pub fn finish_ok(self, path: PathBuf) {
    let _ = self.tx.send_replace(Some(Ok(path)));
  }

  /// Publish a finalise-time error. The backend-unsupported path
  /// ([`crate::web_error::ErrorDetails`] doesn't apply here — this is
  /// a process-level failure, not a JS exception) is the typical
  /// caller: a `WebKit` page's `page.video()` accessor still returns
  /// a [`Video`], but its accessors all resolve with a reason string.
  pub fn finish_err(self, error: FerriError) {
    let _ = self.tx.send_replace(Some(Err(error)));
  }
}
