//! Trace recording — `context.tracing.start()` / `stop()` /
//! `startChunk()` / `stopChunk()`.
//!
//! Emits Playwright's trace format VERSION 8 (`packages/trace/src/trace.ts`),
//! so `npx playwright show-trace` / trace.playwright.dev open ferridriver
//! traces directly. A trace zip contains:
//!
//! * `trace.trace` — JSONL; the FIRST line must be a `context-options`
//!   event carrying `version: 8` (the loader assumes v6 otherwise and
//!   mis-modernizes everything, `traceModernizer.ts:195-203`);
//! * `trace.network` — JSONL of `resource-snapshot` events wrapping HAR
//!   entries (bodies referenced by `_sha1` into `resources/`);
//! * `resources/<name>` — screencast JPEG frames
//!   (`<pageId>-<epochMs>.jpeg`) and network bodies (`<sha1>.<ext>`).
//!
//! Actions are emitted as Playwright's split `before` / `input` /
//! `after` triplet (plus `log` lines), exactly like `tracing.ts`:
//! the `before` event is written when the action starts, so a live
//! export (`bdd --ui`) shows in-flight actions and a crashed action
//! still appears in the trace (the loader synthesizes the missing
//! `after`, `traceLoader.ts`). DOM snapshots (`frame-snapshot` events,
//! `beforeSnapshot`/`afterSnapshot` names) are captured around actions
//! by `crate::snapshotter`; console messages and page lifecycle
//! events are fed from the per-page bookkeeping listener
//! (`crate::page::Page::seed_frame_cache`).

mod actions;
mod events;
mod recorder;
#[cfg(test)]
mod tests;

pub use actions::{
  ActionGate, ActionInfo, ActionObserver, ActionSpan, CallOrigin, CustomAction, OwnerObserverGuard, PendingCall,
  SessionObserverGuard, begin_custom_action, call_origin_here, call_origins_wanted, clear_action_gate,
  forget_pending_calls, observe_owner_actions, observe_session_actions, pending_calls, record_error, set_action_gate,
  set_action_observer, track_pending_calls, with_call_origin, with_call_owner, within_assertion,
};
pub(crate) use actions::{
  InputKind, begin_action, has_action_observer_or_gate, input_action_kind, open_action, within_action,
};
pub use events::{
  ActionErrorInfo, AfterActionEvent, BeforeActionEvent, ConsoleEvent, InputActionEvent, LogEvent, PageEventEntry,
  ScreencastFrameEvent, StackFrame, StdioEvent, TraceAttachment, TraceErrorEvent, TraceEvent, TraceLocation,
  TraceResource, TraceStreaming, TracingChunkOptions, TracingStartOptions, TracingStopOptions,
};
pub use recorder::{TraceRecorder, sweep_stale_spools};

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::error::{FerriError, Result};

// ── Process-global recorder registry ───────────────────────────────────
//
// Keyed by composite session key. Process-global (not a BrowserState
// field) because the action hot paths (locator retry loop, page.goto,
// ...) need a SYNC, contention-free lookup — they cannot take the
// state's tokio RwLock, and a `try_read` miss would silently drop
// actions from the trace.

// RwLock: `recorder_for` runs on every action across ALL parallel
// workers — concurrent read probes must not serialize on a Mutex.
// Writes (install/take) happen twice per recording.
static RECORDERS: std::sync::LazyLock<std::sync::RwLock<rustc_hash::FxHashMap<String, Arc<TraceRecorder>>>> =
  std::sync::LazyLock::new(|| std::sync::RwLock::new(rustc_hash::FxHashMap::default()));

/// True while any composite is being recorded. Lets [`call_origins_wanted`]
/// answer without taking the recorder lock — it is asked once per API call
/// from the host language, including in processes that never trace.
pub(super) static RECORDING_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Install a recorder for `composite`. Errors if one is already active.
pub(crate) fn install_recorder(composite: &str, recorder: Arc<TraceRecorder>) -> Result<()> {
  let mut guard = RECORDERS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
  if guard.contains_key(composite) {
    return Err(FerriError::backend("Tracing has been already started".to_string()));
  }
  guard.insert(composite.to_string(), recorder);
  RECORDING_ACTIVE.store(true, Ordering::Release);
  Ok(())
}

/// The active recorder for `composite`, if tracing.
#[must_use]
pub(crate) fn recorder_for(composite: &str) -> Option<Arc<TraceRecorder>> {
  if !RECORDING_ACTIVE.load(Ordering::Acquire) {
    return None;
  }
  RECORDERS
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .get(composite)
    .cloned()
}

/// Remove and return the recorder for `composite`.
pub(crate) fn take_recorder(composite: &str) -> Option<Arc<TraceRecorder>> {
  let mut guard = RECORDERS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
  let taken = guard.remove(composite);
  RECORDING_ACTIVE.store(!guard.is_empty(), Ordering::Release);
  taken
}

/// Result of a live-trace snapshot request (see
/// [`export_live_snapshot`]).
pub enum LiveTraceSnapshot {
  /// `composite` is not being traced (not started yet, or stopped).
  NotRecording,
  /// The spool has not grown past `known_version` — the caller's cached
  /// bytes are still current; no export was performed.
  Unchanged(u64),
  /// A fresh zip snapshot of the in-progress recording, plus the spool
  /// version it captures.
  Zip { version: u64, bytes: Vec<u8> },
}

/// Export a SNAPSHOT of the in-progress recording for `composite` as an
/// in-memory Playwright-compatible `trace.zip`, without stopping the
/// recording. Non-destructive: the spool keeps growing after this
/// returns. Powers the `bdd --ui` live-trace view — a poller exports
/// the current trace repeatedly while the test runs and feeds each zip
/// to the embedded viewer. Pollers pass the version of their cached
/// snapshot back as `known_version` and get
/// [`LiveTraceSnapshot::Unchanged`] without a re-export when nothing
/// changed.
///
/// Network entries are intentionally empty: the HAR entries are built
/// from the context's network log at `stop`, which this free function
/// (no context handle) cannot reach — so the viewer's Network tab is
/// empty in the live view and fills once the finished trace loads.
///
/// # Errors
///
/// Errors when the zip write fails.
pub fn export_live_snapshot(composite: &str, known_version: Option<u64>) -> Result<LiveTraceSnapshot> {
  let Some(recorder) = recorder_for(composite) else {
    return Ok(LiveTraceSnapshot::NotRecording);
  };
  let version = recorder.spool_version();
  if known_version == Some(version) {
    return Ok(LiveTraceSnapshot::Unchanged(version));
  }
  let bytes = recorder.export_to_vec(&[])?;
  Ok(LiveTraceSnapshot::Zip { version, bytes })
}

// ── Screencast pump ────────────────────────────────────────────────────

/// Steady-state screencast cap: 1 frame / 200ms (Playwright's
/// `throttledRate`, `tracing.ts:783`).
const MIN_FRAME_GAP_MS: f64 = 200.0;
/// Around-action burst: every action boundary lifts the throttle for
/// this long (Playwright's `unthrottleDuration`, `tracing.ts:784`).
pub(super) const SCREENCAST_BURST_MS: u64 = 500;

/// The `page@<id>` identity used for a page's trace events. Derived
/// from the backend page's frame-cache Arc — the same pointer
/// [`crate::page::Page::backend_page_id`] hashes — so screencast
/// frames, console events, and action `pageId`s all correlate in the
/// viewer.
pub(crate) fn trace_page_id(page: &crate::backend::AnyPage) -> String {
  format!("page@{}", Arc::as_ptr(page.frame_cache()).cast::<()>() as usize)
}

/// Subscribe to `page`'s screencast and pump JPEG frames into the trace's
/// film strip. Failure to start (backend without screencast) degrades to
/// a trace without frames for that page.
pub(crate) async fn spawn_screencast_pump(recorder: &Arc<TraceRecorder>, page: &crate::backend::AnyPage) {
  // A just-failed main-frame navigation leaves the CDP session
  // transiently "Not attached to an active page"; the state self-heals
  // within milliseconds, so retry briefly instead of silently
  // recording a frameless trace.
  let params = crate::backend::screencast::ScreencastParams::new(70, 800, 600);
  let mut attempt = 0;
  let (subscription, mut rx) = loop {
    match page.subscribe_screencast(params).await {
      Ok(started) => break started,
      Err(e) if attempt < 5 => {
        attempt += 1;
        tokio::time::sleep(std::time::Duration::from_millis(50 * attempt)).await;
        tracing::debug!(target: "ferridriver::trace", "start_screencast attempt {attempt} failed: {e}");
      },
      Err(e) => {
        tracing::warn!(target: "ferridriver::trace", "screencast unavailable for trace: {e}");
        return;
      },
    }
  };
  recorder.track_screencast(subscription);
  let page_id = trace_page_id(page);
  let recorder = Arc::clone(recorder);
  tokio::spawn(async move {
    let mut last_ts = f64::NEG_INFINITY;
    while let Some(frame) = rx.recv().await {
      let jpeg = frame.data;
      let timestamp = recorder.monotonic_ms();
      if timestamp - last_ts < MIN_FRAME_GAP_MS && !recorder.screencast_burst_active() {
        continue;
      }
      last_ts = timestamp;
      let (width, height) = jpeg_dimensions(&jpeg).unwrap_or((800, 600));
      let name = format!("{page_id}-{:.0}.jpeg", recorder.wall_ms().floor());
      recorder.push_resource(&TraceResource {
        name: name.clone(),
        bytes: jpeg,
      });
      recorder.push_event(&TraceEvent::ScreencastFrame(ScreencastFrameEvent {
        page_id: page_id.clone(),
        resource_name: name,
        width,
        height,
        timestamp,
        frame_swap_wall_time: recorder.wall_ms(),
      }));
    }
  });
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
  image::ImageReader::new(std::io::Cursor::new(bytes))
    .with_guessed_format()
    .ok()?
    .into_dimensions()
    .ok()
}

// ── Page-event recording ───────────────────────────────────────────────

/// Mirror a user-visible page event into the trace: console messages
/// become `console` lines (the viewer's Console tab), page lifecycle
/// (dialog / download / pageError / close) becomes `event` lines on
/// the timeline. Shapes mirror `tracing.ts::_onConsoleMessage` /
/// `onDialog` / `onDownload` / `onPageClose` / `_onPageError`. Fed
/// from the per-page bookkeeping listener — a lossless emitter
/// subscription, so an event storm cannot drop trace lines.
pub(crate) fn record_page_event(recorder: &Arc<TraceRecorder>, page_id: &str, event: &crate::events::PageEvent) {
  use crate::events::PageEvent;
  let time = recorder.monotonic_ms();
  match event {
    PageEvent::Console(msg) => {
      let loc = msg.location();
      recorder.push_event(&TraceEvent::Console(ConsoleEvent {
        time,
        message_type: msg.type_str().to_string(),
        text: msg.text().to_string(),
        page_id: Some(page_id.to_string()),
        url: loc.url.clone(),
        line_number: loc.line_number,
        column_number: loc.column_number,
        args: msg.trace_args(),
      }));
    },
    PageEvent::PageError(err) => {
      let details = err.error();
      let location = err.location();
      recorder.push_event(&TraceEvent::PageEvent(PageEventEntry {
        time,
        method: "pageError".to_string(),
        params: serde_json::json!({
          "error": {
            "error": {
              "name": details.name,
              "message": details.message,
              "stack": details.stack,
            },
          },
          "location": {
            "url": location.url,
            "line": location.line_number,
            "column": location.column_number,
          },
        }),
        page_id: Some(page_id.to_string()),
      }));
    },
    PageEvent::Dialog(dialog) => {
      recorder.push_event(&TraceEvent::PageEvent(PageEventEntry {
        time,
        method: "dialog".to_string(),
        params: serde_json::json!({
          "pageId": page_id,
          "type": dialog.dialog_type().as_str(),
          "message": dialog.message(),
          "defaultValue": dialog.default_value(),
        }),
        page_id: Some(page_id.to_string()),
      }));
    },
    PageEvent::Download(download) => {
      recorder.push_event(&TraceEvent::PageEvent(PageEventEntry {
        time,
        method: "download".to_string(),
        params: serde_json::json!({
          "pageId": page_id,
          "url": download.url(),
          "suggestedFilename": download.suggested_filename(),
        }),
        page_id: Some(page_id.to_string()),
      }));
    },
    PageEvent::Close => {
      recorder.push_event(&TraceEvent::PageEvent(PageEventEntry {
        time,
        method: "pageClosed".to_string(),
        params: serde_json::json!({ "pageId": page_id }),
        page_id: Some(page_id.to_string()),
      }));
    },
    _ => {},
  }
}

/// Record the `page` lifecycle event for a page opened while tracing
/// (mirrors `tracing.ts::onPageOpen`).
pub(crate) fn record_page_open(recorder: &Arc<TraceRecorder>, page_id: &str) {
  recorder.push_event(&TraceEvent::PageEvent(PageEventEntry {
    time: recorder.monotonic_ms(),
    method: "page".to_string(),
    params: serde_json::json!({ "pageId": page_id }),
    page_id: Some(page_id.to_string()),
  }));
}
