//! Video recording via CDP screencast + in-process ffmpeg encoding.
//!
//! Architecture: encoding runs on a blocking thread CONCURRENTLY with the test,
//! driven by a bounded channel from the CDP screencast pump. No subprocess spawning.
//!
//! **Eager** (`start_recording`): encoder thread runs during the test.
//! **Deferred** (`start_buffered_recording`): buffers frames, encodes only if needed.
//!
//! Shutdown is natural: `stop_screencast` -> pump sees channel close -> encoder drains
//! remaining frames -> finishes. No abort, no hang.
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

use tokio::sync::{Mutex, mpsc, watch};

use crate::Page;
use crate::error::FerriError;

const FPS: u32 = 25;

pub use crate::ffmpeg::{video_content_type, video_extension};

// ── Eager recording: encode concurrently with the test ──

pub struct VideoRecordingHandle {
  /// Pump task: CDP screencast -> channel.
  pump_task: tokio::task::JoinHandle<()>,
  /// Encoder task: channel -> ffmpeg encode (blocking thread).
  encoder_task: tokio::task::JoinHandle<crate::error::Result<()>>,
  /// Cooperative shutdown signal to the CDP screencast listener.
  /// Sending `()` causes the listener to drain any buffered
  /// `screencastFrame` events, forward them through the pump, then
  /// drop the sender so the encoder sees end-of-stream. No sleep,
  /// no abort, no lost frames.
  shutdown_tx: tokio::sync::oneshot::Sender<()>,
  output_path: PathBuf,
}

/// Start recording. Encoding runs concurrently on a blocking thread.
///
/// # Errors
///
/// Returns an error if the CDP screencast cannot be started.
pub async fn start_recording(
  page: &Page,
  output_path: PathBuf,
  width: u32,
  height: u32,
  quality: u8,
) -> crate::error::Result<VideoRecordingHandle> {
  let w = width & !1;
  let h = height & !1;

  // Bounded channel: CDP pump -> encoder. Backpressure prevents unbounded buffering.
  let (frame_tx, frame_rx) = mpsc::channel::<(Vec<u8>, f64)>(64);

  // Start CDP screencast. `shutdown_tx` drives cooperative teardown
  // of the listener; we hold onto it until `stop()` is called.
  let (cdp_rx, shutdown_tx) = page.start_screencast(quality, w, h).await?;

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
    shutdown_tx,
    output_path,
  })
}

impl VideoRecordingHandle {
  /// Stop recording: stop screencast, wait for encoder to finish.
  ///
  /// # Errors
  ///
  /// Returns an error if the encoder fails or the join handle panics.
  pub async fn stop(self, page: &Page) -> crate::error::Result<PathBuf> {
    // 1. Tell Chrome to stop emitting `Page.screencastFrame`. The
    //    recording task only runs `stop()` after `page.is_closed()`
    //    returns true, so the target is already gone here on most
    //    callers — the send_command would either error fast or, on
    //    cdp-raw, sit on the response-waiting oneshot until the per-
    //    command 30s transport timeout. Skip the round-trip when the
    //    page is already closed; the shutdown_tx + pump tear-down
    //    below makes the stop deterministic regardless.
    if !page.is_closed() {
      let _ = page.stop_screencast().await;
    }

    // 2. Signal the listener to drain any events already buffered in
    //    its broadcast subscription (frames Chrome shipped just
    //    before the stop took effect). When the listener finishes
    //    draining, it drops the channel sender.
    let _ = self.shutdown_tx.send(());

    // 3. The pump's `rx.recv()` returns `None` once the listener
    //    drops its sender. Await the pump rather than aborting -- no
    //    frames lost, no arbitrary sleeps.
    let _ = self.pump_task.await;

    // 4. Pump exit drops `frame_tx`, encoder drains the bounded
    //    channel and finishes.
    self
      .encoder_task
      .await
      .map_err(|e| FerriError::Backend(format!("encoder join: {e}")))??;

    Ok(self.output_path)
  }
}

// ── Deferred (buffered) recording: zero encoding cost for passing tests ──

/// A timestamped JPEG frame: `(jpeg_bytes, timestamp_secs)`.
type FrameBuffer = Arc<Mutex<Vec<(Vec<u8>, f64)>>>;

pub struct BufferedRecordingHandle {
  frames: FrameBuffer,
  pump_task: tokio::task::JoinHandle<()>,
  shutdown_tx: tokio::sync::oneshot::Sender<()>,
  width: u32,
  height: u32,
}

/// Start buffered recording: buffer frames in memory, no encoding until requested.
///
/// # Errors
///
/// Returns an error if the CDP screencast cannot be started.
pub async fn start_buffered_recording(
  page: &Page,
  width: u32,
  height: u32,
  quality: u8,
) -> crate::error::Result<BufferedRecordingHandle> {
  let w = width & !1;
  let h = height & !1;

  let (cdp_rx, shutdown_tx) = page.start_screencast(quality, w, h).await?;
  let frames: FrameBuffer = Arc::new(Mutex::new(Vec::with_capacity(64)));

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
    shutdown_tx,
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
  pub async fn encode(self, page: &Page, output_path: PathBuf) -> crate::error::Result<PathBuf> {
    let _ = page.stop_screencast().await;
    let _ = self.shutdown_tx.send(());
    let _ = self.pump_task.await;

    let frames = self.frames.lock().await;
    if frames.is_empty() {
      return Err(FerriError::backend("no frames captured"));
    }

    let w = self.width;
    let h = self.height;
    let frames_owned: Vec<(Vec<u8>, f64)> = frames.clone();
    drop(frames);

    let path = output_path.clone();
    tokio::task::spawn_blocking(move || crate::ffmpeg::encode_frames(&frames_owned, &path, w, h, FPS))
      .await
      .map_err(|e| FerriError::backend(format!("encode join: {e}")))??;

    Ok(output_path)
  }

  /// Stop capturing and discard frames. Zero encoding cost.
  pub async fn discard(self, page: &Page) {
    let _ = page.stop_screencast().await;
    let _ = self.shutdown_tx.send(());
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
    if let Some(parent) = dest.parent() {
      if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
          .map_err(|e| FerriError::Backend(format!("video.saveAs create parent {}: {e}", parent.display())))?;
      }
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
