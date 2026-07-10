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
//! Actions are emitted as single merged `action` events (before+after
//! fields in one line) — fully supported at v8
//! (`traceModernizer.ts:141-144`) and immune to the loader's
//! orphaned-`after` crash. DOM snapshots (`frame-snapshot` events,
//! `beforeSnapshot`/`afterSnapshot` names) are not yet captured: the
//! viewer shows the action list, film strip, console, network, and
//! errors tabs; the snapshot pane renders blank.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::error::{FerriError, Result};

/// Trace format version this recorder emits.
const TRACE_VERSION: u32 = 8;

/// Options bag for `tracing.start` (Playwright:
/// `tracing.start({ name?, title?, screenshots?, snapshots?, sources? })`).
#[derive(Default, Clone)]
pub struct TracingStartOptions {
  /// Prefix for intermediate artifacts (accepted for parity; the zip is
  /// written to the `stop({ path })` location).
  pub name: Option<String>,
  /// Trace title shown in the viewer.
  pub title: Option<String>,
  /// Capture screencast frames into the film strip.
  pub screenshots: bool,
  /// Accepted for parity; DOM snapshots are not captured yet.
  pub snapshots: bool,
  /// Accepted for parity; source files are not embedded yet.
  pub sources: bool,
}

/// Options bag for `tracing.stop` / `tracing.stopChunk`.
#[derive(Default, Clone)]
pub struct TracingStopOptions {
  /// Where to write the `trace.zip`. Without a path the recording is
  /// discarded (Playwright semantics).
  pub path: Option<std::path::PathBuf>,
}

/// One recorded protocol/action event, ready for JSONL serialization.
#[derive(Clone)]
pub enum TraceEvent {
  /// Merged before+after action (`trace.ts` `action` type).
  Action(ActionEvent),
  /// Console message (`console` type).
  Console(ConsoleEvent),
  /// Page lifecycle event shown on the timeline (`event` type).
  PageEvent(PageEventEntry),
  /// Screencast frame reference (`screencast-frame` type).
  ScreencastFrame(ScreencastFrameEvent),
}

#[derive(Clone)]
pub struct ActionEvent {
  pub call_id: String,
  pub start_time: f64,
  pub end_time: f64,
  pub class: String,
  pub method: String,
  pub title: String,
  pub params: serde_json::Value,
  pub error: Option<String>,
  pub page_id: Option<String>,
  /// Call id of the enclosing action (nests actions in the viewer's
  /// tree, e.g. test steps under their parent step).
  pub parent_id: Option<String>,
}

#[derive(Clone)]
pub struct ConsoleEvent {
  pub time: f64,
  pub message_type: String,
  pub text: String,
  pub page_id: Option<String>,
  pub url: String,
  pub line_number: u32,
  pub column_number: u32,
}

#[derive(Clone)]
pub struct PageEventEntry {
  pub time: f64,
  pub method: String,
  pub params: serde_json::Value,
  pub page_id: Option<String>,
}

#[derive(Clone)]
pub struct ScreencastFrameEvent {
  pub page_id: String,
  /// Resource file name inside the zip (`resources/<name>`); the trace
  /// event references it via its `sha1` field (the recorder uses
  /// `<pageId>-<epochMs>.jpeg` names exactly like Playwright,
  /// `tracing.ts:670-689`).
  pub resource_name: String,
  pub width: u32,
  pub height: u32,
  pub timestamp: f64,
  pub frame_swap_wall_time: f64,
}

/// A body payload captured for the trace (screencast frame or network
/// body), written under `resources/` at export.
pub struct TraceResource {
  pub name: String,
  pub bytes: Vec<u8>,
}

/// Live trace recorder, stored per-context on
/// [`crate::state::BrowserState`] between `tracing.start` and
/// `tracing.stop`. All interior mutability is sync — the action hot
/// path appends under a brief mutex.
pub struct TraceRecorder {
  /// Monotonic origin: event times are milliseconds since this instant.
  origin: Instant,
  /// Wall-clock anchor paired with `origin` (epoch ms).
  wall_origin: f64,
  /// Trace title (`context-options.title`).
  title: Option<String>,
  /// Whether screencast frames are being captured.
  pub screenshots: bool,
  /// Chunk-local recorded events, in order.
  events: std::sync::Mutex<Vec<TraceEvent>>,
  /// Chunk-local captured resources.
  resources: std::sync::Mutex<Vec<TraceResource>>,
  /// Network-log length at chunk start — `stop` serializes entries
  /// appended after this point.
  pub network_start_len: AtomicU64,
  /// Monotonic action-id source (`call@N`).
  next_call_id: AtomicU64,
  /// Shutdown senders for per-page screencast pumps.
  screencast_stops: std::sync::Mutex<Vec<tokio::sync::oneshot::Sender<()>>>,
  /// Browser name recorded in `context-options`.
  browser_name: String,
}

impl TraceRecorder {
  #[must_use]
  pub fn new(options: &TracingStartOptions, browser_name: String, network_len: usize) -> Self {
    Self {
      origin: Instant::now(),
      wall_origin: now_epoch_ms(),
      title: options.title.clone(),
      screenshots: options.screenshots,
      events: std::sync::Mutex::new(Vec::new()),
      resources: std::sync::Mutex::new(Vec::new()),
      network_start_len: AtomicU64::new(network_len as u64),
      next_call_id: AtomicU64::new(1),
      screencast_stops: std::sync::Mutex::new(Vec::new()),
      browser_name,
    }
  }

  /// Milliseconds since the recorder's monotonic origin.
  #[must_use]
  pub fn monotonic_ms(&self) -> f64 {
    self.origin.elapsed().as_secs_f64() * 1000.0
  }

  /// Epoch milliseconds (for `frameSwapWallTime` etc).
  #[must_use]
  pub fn wall_ms(&self) -> f64 {
    self.wall_origin + self.monotonic_ms()
  }

  /// Allocate the next `call@N` action id.
  #[must_use]
  pub fn next_call_id(&self) -> String {
    format!("call@{}", self.next_call_id.fetch_add(1, Ordering::Relaxed))
  }

  pub fn push_event(&self, event: TraceEvent) {
    self
      .events
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(event);
  }

  pub fn push_resource(&self, resource: TraceResource) {
    self
      .resources
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(resource);
  }

  /// Track a screencast pump's shutdown sender so `stop` can end it.
  pub fn track_screencast_stop(&self, tx: tokio::sync::oneshot::Sender<()>) {
    self
      .screencast_stops
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(tx);
  }

  /// Reset chunk-local state (`tracing.startChunk` — network sha1s
  /// persist in Playwright, but chunk events/resources restart).
  pub fn start_chunk(&self, network_len: usize) {
    self
      .events
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    self
      .resources
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
    self.network_start_len.store(network_len as u64, Ordering::SeqCst);
  }

  /// Stop screencast pumps (idempotent).
  pub fn stop_screencasts(&self) {
    let stops: Vec<_> = std::mem::take(
      &mut *self
        .screencast_stops
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for tx in stops {
      let _ = tx.send(());
    }
  }

  /// Serialize and write the chunk as a Playwright-compatible
  /// `trace.zip` at `path`.
  ///
  /// # Errors
  ///
  /// Errors if serialization or the zip write fails.
  pub fn export(&self, path: &std::path::Path, network_entries: &[serde_json::Value]) -> Result<()> {
    use std::io::Write;

    let mut trace_lines: Vec<String> = Vec::new();
    trace_lines.push(self.context_options_line());

    let events = self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for event in events.iter() {
      trace_lines.push(serialize_event(event));
    }
    drop(events);

    let mut network_lines: Vec<String> = Vec::new();
    for entry in network_entries {
      let wrapped = serde_json::json!({ "type": "resource-snapshot", "snapshot": entry });
      network_lines.push(wrapped.to_string());
    }

    let file = std::fs::File::create(path)
      .map_err(|e| FerriError::backend(format!("create trace zip {}: {e}", path.display())))?;
    let mut writer = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let zip_err = |e: zip::result::ZipError| FerriError::backend(format!("write trace zip: {e}"));

    writer.start_file("trace.trace", opts).map_err(zip_err)?;
    writer
      .write_all(trace_lines.join("\n").as_bytes())
      .map_err(|e| FerriError::backend(format!("write trace zip: {e}")))?;

    writer.start_file("trace.network", opts).map_err(zip_err)?;
    writer
      .write_all(network_lines.join("\n").as_bytes())
      .map_err(|e| FerriError::backend(format!("write trace zip: {e}")))?;

    let resources = self.resources.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut written = rustc_hash::FxHashSet::default();
    for resource in resources.iter() {
      if !written.insert(resource.name.clone()) {
        continue;
      }
      writer
        .start_file(format!("resources/{}", resource.name), opts)
        .map_err(zip_err)?;
      writer
        .write_all(&resource.bytes)
        .map_err(|e| FerriError::backend(format!("write trace zip: {e}")))?;
    }
    drop(resources);

    writer.finish().map_err(zip_err)?;
    Ok(())
  }

  fn context_options_line(&self) -> String {
    serde_json::json!({
      "version": TRACE_VERSION,
      "type": "context-options",
      "origin": "library",
      "browserName": self.browser_name,
      "platform": std::env::consts::OS,
      "wallTime": self.wall_origin,
      "monotonicTime": 0.0,
      "title": self.title.clone().unwrap_or_default(),
      "options": {},
      "sdkLanguage": "javascript",
    })
    .to_string()
  }
}

fn serialize_event(event: &TraceEvent) -> String {
  match event {
    TraceEvent::Action(a) => serde_json::json!({
      "type": "action",
      "callId": a.call_id,
      "startTime": a.start_time,
      "endTime": a.end_time,
      "class": a.class,
      "method": a.method,
      "title": a.title,
      "params": a.params,
      "error": a.error.as_ref().map(|message| serde_json::json!({
        "name": "Error",
        "message": message,
      })),
      "pageId": a.page_id,
      "parentId": a.parent_id,
    })
    .to_string(),
    TraceEvent::Console(c) => serde_json::json!({
      "type": "console",
      "time": c.time,
      "messageType": c.message_type,
      "text": c.text,
      "pageId": c.page_id,
      "location": {
        "url": c.url,
        "lineNumber": c.line_number,
        "columnNumber": c.column_number,
      },
    })
    .to_string(),
    TraceEvent::PageEvent(e) => serde_json::json!({
      "type": "event",
      "time": e.time,
      "class": "BrowserContext",
      "method": e.method,
      "params": e.params,
      "pageId": e.page_id,
    })
    .to_string(),
    TraceEvent::ScreencastFrame(f) => serde_json::json!({
      "type": "screencast-frame",
      "pageId": f.page_id,
      "sha1": f.resource_name,
      "width": f.width,
      "height": f.height,
      "timestamp": f.timestamp,
      "frameSwapWallTime": f.frame_swap_wall_time,
    })
    .to_string(),
  }
}

fn now_epoch_ms() -> f64 {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  now.as_secs_f64() * 1000.0
}

// ── Process-global recorder registry ───────────────────────────────────
//
// Keyed by composite session key. Process-global (not a BrowserState
// field) because the action hot paths (locator retry loop, page.goto,
// ...) need a SYNC, contention-free lookup — they cannot take the
// state's tokio RwLock, and a `try_read` miss would silently drop
// actions from the trace.

static RECORDERS: std::sync::LazyLock<std::sync::Mutex<rustc_hash::FxHashMap<String, Arc<TraceRecorder>>>> =
  std::sync::LazyLock::new(|| std::sync::Mutex::new(rustc_hash::FxHashMap::default()));

/// Install a recorder for `composite`. Errors if one is already active.
pub(crate) fn install_recorder(composite: &str, recorder: Arc<TraceRecorder>) -> Result<()> {
  let mut guard = RECORDERS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  if guard.contains_key(composite) {
    return Err(FerriError::backend("Tracing has been already started".to_string()));
  }
  guard.insert(composite.to_string(), recorder);
  Ok(())
}

/// The active recorder for `composite`, if tracing.
#[must_use]
pub(crate) fn recorder_for(composite: &str) -> Option<Arc<TraceRecorder>> {
  RECORDERS
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .get(composite)
    .cloned()
}

/// Remove and return the recorder for `composite`.
pub(crate) fn take_recorder(composite: &str) -> Option<Arc<TraceRecorder>> {
  RECORDERS
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .remove(composite)
}

// ── Screencast pump ────────────────────────────────────────────────────

static NEXT_TRACE_PAGE_ID: AtomicU64 = AtomicU64::new(1);

/// Start a screencast on `page` and pump JPEG frames into the trace's
/// film strip. Failure to start (backend without screencast, video
/// recording already holding the stream) degrades to a trace without
/// frames for that page.
pub(crate) async fn spawn_screencast_pump(recorder: &Arc<TraceRecorder>, page: &crate::backend::AnyPage) {
  // Throttle mirrors Playwright's steady-state cap (1 frame / 200ms,
  // `tracing.ts:783-837`); the around-action burst window is not
  // implemented.
  const MIN_FRAME_GAP_MS: f64 = 200.0;

  let Ok((mut rx, stop_tx)) = page.start_screencast(70, 800, 600).await else {
    return;
  };
  recorder.track_screencast_stop(stop_tx);
  let page_id = format!("page@{}", NEXT_TRACE_PAGE_ID.fetch_add(1, Ordering::Relaxed));
  let recorder = Arc::clone(recorder);
  tokio::spawn(async move {
    let mut last_ts = f64::NEG_INFINITY;
    while let Some((jpeg, _backend_ts)) = rx.recv().await {
      let timestamp = recorder.monotonic_ms();
      if timestamp - last_ts < MIN_FRAME_GAP_MS {
        continue;
      }
      last_ts = timestamp;
      let (width, height) = jpeg_dimensions(&jpeg).unwrap_or((800, 600));
      // Epoch-ms wall clock: positive and below 2^53, exact as u64.
      #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
      let name = format!("{page_id}-{}.jpeg", recorder.wall_ms() as u64);
      recorder.push_resource(TraceResource {
        name: name.clone(),
        bytes: jpeg,
      });
      recorder.push_event(TraceEvent::ScreencastFrame(ScreencastFrameEvent {
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

// ── Action spans ───────────────────────────────────────────────────────

/// An in-flight traced action. Created by [`begin_action`] at an action
/// funnel; [`ActionSpan::finish`] emits the merged `action` event.
pub struct ActionSpan {
  recorder: Arc<TraceRecorder>,
  call_id: String,
  start_time: f64,
  class: &'static str,
  method: String,
  title: String,
  params: serde_json::Value,
  page_id: Option<String>,
  parent_id: Option<String>,
}

impl ActionSpan {
  /// The span's `call@N` id — pass as `parent_id` of child spans to
  /// nest them under this action in the viewer.
  #[must_use]
  pub fn call_id(&self) -> &str {
    &self.call_id
  }

  /// Emit the action event, recording `error` when the action failed.
  pub fn finish(self, error: Option<&FerriError>) {
    self.finish_message(error.map(std::string::ToString::to_string));
  }

  /// Emit the action event with an already-stringified error (spans
  /// opened by external runners carry plain-text failures).
  pub fn finish_message(self, error: Option<String>) {
    self.finish_message_ended_ago(error, 0.0);
  }

  /// Emit the action event with the end time backdated by
  /// `ended_ms_ago` (steps recorded after the fact, e.g. a scenario
  /// runner that reports all step results at scenario end).
  pub fn finish_message_ended_ago(self, error: Option<String>, ended_ms_ago: f64) {
    let end_time = (self.recorder.monotonic_ms() - ended_ms_ago).max(self.start_time);
    self.recorder.push_event(TraceEvent::Action(ActionEvent {
      call_id: self.call_id,
      start_time: self.start_time,
      end_time,
      class: self.class.to_string(),
      method: self.method,
      title: self.title,
      params: self.params,
      error,
      page_id: self.page_id,
      parent_id: self.parent_id,
    }));
  }
}

/// Start a traced action span when `composite` has an active recorder.
/// Cheap when tracing is off (one mutex-protected map probe).
#[must_use]
pub(crate) fn begin_action(
  composite: Option<&str>,
  class: &'static str,
  method: &str,
  page_id: Option<String>,
  params: serde_json::Value,
) -> Option<ActionSpan> {
  let recorder = recorder_for(composite?)?;
  let start_time = recorder.monotonic_ms();
  let call_id = recorder.next_call_id();
  let title = format!("{}.{method}", class.to_ascii_lowercase());
  Some(ActionSpan {
    recorder,
    call_id,
    start_time,
    class,
    method: method.to_string(),
    title,
    params,
    page_id,
    parent_id: None,
  })
}

/// Open a titled action span on the active recorder for `composite`.
/// Entry point for external runners injecting non-protocol actions
/// (test-runner step boundaries) into a trace: the runner supplies the
/// display title, an optional parent call id for nesting, and a
/// `backdate_ms` for spans recorded after the fact. Returns `None`
/// when the composite is not being traced.
#[must_use]
pub fn begin_custom_action(
  composite: &str,
  class: &'static str,
  method: &str,
  title: String,
  params: serde_json::Value,
  parent_id: Option<String>,
  backdate_ms: f64,
) -> Option<ActionSpan> {
  let recorder = recorder_for(composite)?;
  let start_time = (recorder.monotonic_ms() - backdate_ms).max(0.0);
  let call_id = recorder.next_call_id();
  Some(ActionSpan {
    recorder,
    call_id,
    start_time,
    class,
    method: method.to_string(),
    title,
    params,
    page_id: None,
    parent_id,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn context_options_is_first_line_with_version_8() {
    let recorder = TraceRecorder::new(&TracingStartOptions::default(), "chromium".into(), 0);
    let line = recorder.context_options_line();
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["version"].as_u64(), Some(8));
    assert_eq!(parsed["type"].as_str(), Some("context-options"));
    assert_eq!(parsed["origin"].as_str(), Some("library"));
  }

  #[test]
  fn action_event_serializes_v8_merged_shape() {
    let line = serialize_event(&TraceEvent::Action(ActionEvent {
      call_id: "call@1".into(),
      start_time: 1.0,
      end_time: 2.0,
      class: "Frame".into(),
      method: "click".into(),
      title: "click".into(),
      params: serde_json::json!({ "selector": "#a" }),
      error: None,
      page_id: Some("page@1".into()),
      parent_id: None,
    }));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("action"));
    assert_eq!(parsed["callId"].as_str(), Some("call@1"));
    assert!(parsed["startTime"].as_f64().unwrap() < parsed["endTime"].as_f64().unwrap());
  }

  #[test]
  fn export_writes_required_zip_entries() {
    let dir = std::env::temp_dir().join(format!("ferri-trace-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.zip");
    let recorder = TraceRecorder::new(&TracingStartOptions::default(), "chromium".into(), 0);
    recorder.push_event(TraceEvent::Action(ActionEvent {
      call_id: recorder.next_call_id(),
      start_time: recorder.monotonic_ms(),
      end_time: recorder.monotonic_ms(),
      class: "Page".into(),
      method: "goto".into(),
      title: "page.goto".into(),
      params: serde_json::json!({ "url": "about:blank" }),
      error: None,
      page_id: None,
      parent_id: None,
    }));
    recorder.push_resource(TraceResource {
      name: "page@1-1.jpeg".into(),
      bytes: vec![0xFF, 0xD8],
    });
    recorder
      .export(&path, &[serde_json::json!({ "request": {}, "response": {} })])
      .unwrap();

    let file = std::fs::File::open(&path).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let names: Vec<String> = (0..archive.len())
      .map(|i| archive.by_index(i).unwrap().name().to_string())
      .collect();
    assert!(names.contains(&"trace.trace".to_string()));
    assert!(names.contains(&"trace.network".to_string()));
    assert!(names.contains(&"resources/page@1-1.jpeg".to_string()));

    let mut trace = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("trace.trace").unwrap(), &mut trace).unwrap();
    let first: serde_json::Value = serde_json::from_str(trace.lines().next().unwrap()).unwrap();
    assert_eq!(
      first["version"].as_u64(),
      Some(8),
      "first line must be context-options v8"
    );
    std::fs::remove_dir_all(&dir).ok();
  }
}
