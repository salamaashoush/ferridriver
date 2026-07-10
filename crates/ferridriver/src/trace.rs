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
//! `beforeSnapshot`/`afterSnapshot` names) are captured around actions
//! by [`crate::snapshotter`]; console messages and page lifecycle
//! events are fed from the per-page bookkeeping listener
//! (`crate::page::Page::seed_frame_cache`).

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
  /// Capture DOM snapshots around actions.
  pub snapshots: bool,
  /// Embed each source file referenced by an action's stack frames as a
  /// `resources/src@<sha1>.txt` entry (the viewer's Source tab).
  pub sources: bool,
}

/// One frame of an action's call stack (`trace.ts` `StackFrame`). The
/// viewer's Source tab loads `resources/src@<sha1-of-file-path>.txt`
/// for the top frame's `file` when the trace embeds sources.
#[derive(Clone)]
pub struct StackFrame {
  pub file: String,
  pub line: u32,
  pub column: u32,
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
  /// DOM snapshot of one frame (`frame-snapshot` type). Carries the
  /// fully built snapshot object (see `crate::snapshotter`).
  FrameSnapshot(serde_json::Value),
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
  /// `before@<callId>` snapshot name (viewer's Before pane).
  pub before_snapshot: Option<String>,
  /// `after@<callId>` snapshot name (viewer's After pane).
  pub after_snapshot: Option<String>,
  /// Call-site stack frames (viewer's Source tab / action location).
  pub stack: Vec<StackFrame>,
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
  /// `[{ preview, value }]` per arg (the viewer's Console tab expands
  /// these); empty when the message carried no args.
  pub args: Vec<serde_json::Value>,
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

/// On-disk spool for an in-flight recording: events append to a
/// buffered `trace.trace` JSONL file, resources land under
/// `resources/` as they arrive. Memory stays flat no matter how long
/// the recording runs (screencast frames alone would otherwise grow
/// unbounded); export streams the spool into the final zip.
struct TraceSpool {
  dir: std::path::PathBuf,
  trace: std::io::BufWriter<std::fs::File>,
  /// sha1-style resource names already written (dedup).
  written_resources: rustc_hash::FxHashSet<String>,
}

impl TraceSpool {
  fn create(first_line: &str) -> Result<Self> {
    static NEXT_SPOOL_ID: AtomicU64 = AtomicU64::new(1);
    let dir = std::env::temp_dir().join(format!(
      "ferridriver-trace-{}-{}",
      std::process::id(),
      NEXT_SPOOL_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join("resources"))
      .map_err(|e| FerriError::backend(format!("create trace spool {}: {e}", dir.display())))?;
    let file = std::fs::File::create(dir.join("trace.trace"))
      .map_err(|e| FerriError::backend(format!("create trace spool file: {e}")))?;
    let mut spool = Self {
      dir,
      trace: std::io::BufWriter::new(file),
      written_resources: rustc_hash::FxHashSet::default(),
    };
    spool.write_line(first_line);
    Ok(spool)
  }

  fn write_line(&mut self, line: &str) {
    use std::io::Write;
    let _ = self.trace.write_all(line.as_bytes());
    let _ = self.trace.write_all(b"\n");
  }

  fn write_resource(&mut self, resource: &TraceResource) {
    if !self.written_resources.insert(resource.name.clone()) {
      return;
    }
    let _ = std::fs::write(self.dir.join("resources").join(&resource.name), &resource.bytes);
  }
}

impl Drop for TraceSpool {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

/// Live trace recorder, stored per-context on
/// [`crate::state::BrowserState`] between `tracing.start` and
/// `tracing.stop`. All interior mutability is sync — the action hot
/// path appends a serialized line to the disk spool under a brief
/// mutex.
pub struct TraceRecorder {
  /// Monotonic origin: event times are milliseconds since this instant.
  origin: Instant,
  /// Wall-clock anchor paired with `origin` (epoch ms).
  wall_origin: f64,
  /// Trace title (`context-options.title`).
  title: Option<String>,
  /// Whether screencast frames are being captured.
  pub screenshots: bool,
  /// Whether DOM snapshots are being captured around actions.
  pub snapshots: bool,
  /// Whether source files referenced by action stacks are embedded.
  pub sources: bool,
  /// Monotonic-ms deadline until which the screencast throttle is
  /// lifted (Playwright's around-action burst, `tracing.ts:783-837`:
  /// `temporarilyDisableThrottling` on before/input/after call).
  screencast_burst_until_ms: AtomicU64,
  /// Source files already embedded as `src@<sha1>.txt` resources.
  sources_embedded: std::sync::Mutex<rustc_hash::FxHashSet<String>>,
  /// Chunk-local disk spool (events + resources).
  spool: std::sync::Mutex<TraceSpool>,
  /// Network-log length at chunk start — `stop` serializes entries
  /// appended after this point.
  pub network_start_len: AtomicU64,
  /// Monotonic action-id source (`call@N`).
  next_call_id: AtomicU64,
  /// Call id of the live enclosing span (a test step): actions recorded
  /// while set nest under it in the viewer's tree.
  current_parent: std::sync::Mutex<Option<String>>,
  /// Shutdown senders for per-page screencast pumps.
  screencast_stops: std::sync::Mutex<Vec<tokio::sync::oneshot::Sender<()>>>,
  /// Browser name recorded in `context-options`.
  browser_name: String,
}

impl TraceRecorder {
  /// # Errors
  ///
  /// Errors if the on-disk spool cannot be created.
  pub fn new(options: &TracingStartOptions, browser_name: String, network_len: usize) -> Result<Self> {
    let origin = Instant::now();
    let wall_origin = now_epoch_ms();
    let first_line = context_options_line(&browser_name, wall_origin, options.title.as_deref());
    Ok(Self {
      origin,
      wall_origin,
      title: options.title.clone(),
      screenshots: options.screenshots,
      snapshots: options.snapshots,
      sources: options.sources,
      screencast_burst_until_ms: AtomicU64::new(0),
      sources_embedded: std::sync::Mutex::new(rustc_hash::FxHashSet::default()),
      spool: std::sync::Mutex::new(TraceSpool::create(&first_line)?),
      network_start_len: AtomicU64::new(network_len as u64),
      next_call_id: AtomicU64::new(1),
      current_parent: std::sync::Mutex::new(None),
      screencast_stops: std::sync::Mutex::new(Vec::new()),
      browser_name,
    })
  }

  /// Swap the live enclosing-span id, returning the previous one so the
  /// caller can restore it when its span closes (stack discipline).
  pub fn swap_current_parent(&self, parent: Option<String>) -> Option<String> {
    let mut guard = self
      .current_parent
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::mem::replace(&mut *guard, parent)
  }

  fn current_parent(&self) -> Option<String> {
    self
      .current_parent
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
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

  /// Map a wall-clock epoch-ms sample onto this recorder's monotonic
  /// timeline (`context-options` anchors `wallTime` at monotonic 0).
  #[must_use]
  pub fn monotonic_of_wall_ms(&self, wall_ms: f64) -> f64 {
    wall_ms - self.wall_origin
  }

  /// Allocate the next `call@N` action id.
  #[must_use]
  pub fn next_call_id(&self) -> String {
    format!("call@{}", self.next_call_id.fetch_add(1, Ordering::Relaxed))
  }

  /// Lift the screencast throttle for the next 500ms (mirrors
  /// Playwright's `unthrottleDuration` around every action boundary).
  fn bump_screencast_burst(&self) {
    // Millisecond resolution is plenty for a 500ms window.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let until = (self.monotonic_ms() + SCREENCAST_BURST_MS) as u64;
    self.screencast_burst_until_ms.store(until, Ordering::Relaxed);
  }

  /// Whether the around-action burst window is open at `now_ms`.
  fn screencast_burst_active(&self, now_ms: f64) -> bool {
    #[allow(clippy::cast_precision_loss)]
    let until = self.screencast_burst_until_ms.load(Ordering::Relaxed) as f64;
    now_ms < until
  }

  /// Embed `file` as a `resources/src@<sha1-of-path>.txt` entry (the
  /// viewer's Source tab fetches exactly that name for a stack frame's
  /// `file`, `sourceTab.tsx` / `localUtils.ts:78`). No-op unless the
  /// recording was started with `sources: true`; each file is read
  /// once per recorder; unreadable files are skipped (best effort,
  /// like Playwright's zip-time collection).
  pub fn embed_source(&self, file: &str) {
    if !self.sources {
      return;
    }
    {
      let mut seen = self
        .sources_embedded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if !seen.insert(file.to_string()) {
        return;
      }
    }
    let Ok(bytes) = std::fs::read(file) else {
      return;
    };
    let name = format!("src@{}.txt", crate::tracing::sha1_hex(file.as_bytes()));
    self.push_resource(&TraceResource { name, bytes });
  }

  pub fn push_event(&self, event: &TraceEvent) {
    let line = serialize_event(event);
    self
      .spool
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .write_line(&line);
  }

  pub fn push_resource(&self, resource: &TraceResource) {
    self
      .spool
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .write_resource(resource);
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
  /// persist in Playwright, but chunk events/resources restart): the
  /// old spool is replaced (and its directory removed on drop).
  pub fn start_chunk(&self, network_len: usize) {
    let first_line = context_options_line(&self.browser_name, self.wall_origin, self.title.as_deref());
    if let Ok(fresh) = TraceSpool::create(&first_line) {
      let mut guard = self.spool.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      *guard = fresh;
    }
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

  /// Stream the spooled chunk into a Playwright-compatible `trace.zip`
  /// at `path`. Memory stays flat — the spool files are copied into the
  /// archive, never loaded whole.
  ///
  /// # Errors
  ///
  /// Errors if serialization or the zip write fails.
  pub fn export(&self, path: &std::path::Path, network_entries: &[serde_json::Value]) -> Result<()> {
    use std::io::Write;

    let mut spool = self.spool.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    spool
      .trace
      .flush()
      .map_err(|e| FerriError::backend(format!("flush trace spool: {e}")))?;

    let file = std::fs::File::create(path)
      .map_err(|e| FerriError::backend(format!("create trace zip {}: {e}", path.display())))?;
    let mut writer = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let zip_err = |e: zip::result::ZipError| FerriError::backend(format!("write trace zip: {e}"));
    let io_err = |e: std::io::Error| FerriError::backend(format!("write trace zip: {e}"));

    writer.start_file("trace.trace", opts).map_err(zip_err)?;
    let mut trace_file = std::fs::File::open(spool.dir.join("trace.trace"))
      .map_err(|e| FerriError::backend(format!("open trace spool: {e}")))?;
    std::io::copy(&mut trace_file, &mut writer).map_err(io_err)?;

    writer.start_file("trace.network", opts).map_err(zip_err)?;
    for entry in network_entries {
      let wrapped = serde_json::json!({ "type": "resource-snapshot", "snapshot": entry });
      writer.write_all(wrapped.to_string().as_bytes()).map_err(io_err)?;
      writer.write_all(b"\n").map_err(io_err)?;
    }

    let resources_dir = spool.dir.join("resources");
    let entries =
      std::fs::read_dir(&resources_dir).map_err(|e| FerriError::backend(format!("read trace spool resources: {e}")))?;
    for entry in entries.flatten() {
      let Ok(name) = entry.file_name().into_string() else {
        continue;
      };
      writer.start_file(format!("resources/{name}"), opts).map_err(zip_err)?;
      let mut resource = std::fs::File::open(entry.path())
        .map_err(|e| FerriError::backend(format!("open trace spool resource {name}: {e}")))?;
      std::io::copy(&mut resource, &mut writer).map_err(io_err)?;
    }

    writer.finish().map_err(zip_err)?;
    Ok(())
  }
}

/// First trace line: `context-options` with `version: 8` (the loader
/// mis-modernizes everything as v6 without it).
fn context_options_line(browser_name: &str, wall_origin: f64, title: Option<&str>) -> String {
  serde_json::json!({
    "version": TRACE_VERSION,
    "type": "context-options",
    "origin": "library",
    "browserName": browser_name,
    "platform": std::env::consts::OS,
    "wallTime": wall_origin,
    "monotonicTime": 0.0,
    "title": title.unwrap_or_default(),
    "options": {},
    "sdkLanguage": "javascript",
  })
  .to_string()
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
      "beforeSnapshot": a.before_snapshot,
      "afterSnapshot": a.after_snapshot,
      "stack": a.stack.iter().map(|f| serde_json::json!({
        "file": f.file,
        "line": f.line,
        "column": f.column,
      })).collect::<Vec<_>>(),
    })
    .to_string(),
    TraceEvent::Console(c) => serde_json::json!({
      "type": "console",
      "time": c.time,
      "messageType": c.message_type,
      "text": c.text,
      "args": c.args,
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
    TraceEvent::FrameSnapshot(snapshot) => serde_json::json!({
      "type": "frame-snapshot",
      "snapshot": snapshot,
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

// RwLock: `recorder_for` runs on every action across ALL parallel
// workers — concurrent read probes must not serialize on a Mutex.
// Writes (install/take) happen twice per recording.
static RECORDERS: std::sync::LazyLock<std::sync::RwLock<rustc_hash::FxHashMap<String, Arc<TraceRecorder>>>> =
  std::sync::LazyLock::new(|| std::sync::RwLock::new(rustc_hash::FxHashMap::default()));

/// Install a recorder for `composite`. Errors if one is already active.
pub(crate) fn install_recorder(composite: &str, recorder: Arc<TraceRecorder>) -> Result<()> {
  let mut guard = RECORDERS.write().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .get(composite)
    .cloned()
}

/// Remove and return the recorder for `composite`.
pub(crate) fn take_recorder(composite: &str) -> Option<Arc<TraceRecorder>> {
  RECORDERS
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .remove(composite)
}

// ── Screencast pump ────────────────────────────────────────────────────

/// Steady-state screencast cap: 1 frame / 200ms (Playwright's
/// `throttledRate`, `tracing.ts:783`).
const MIN_FRAME_GAP_MS: f64 = 200.0;
/// Around-action burst: every action boundary lifts the throttle for
/// this long (Playwright's `unthrottleDuration`, `tracing.ts:784`).
const SCREENCAST_BURST_MS: f64 = 500.0;

/// The `page@<id>` identity used for a page's trace events. Derived
/// from the backend page's frame-cache Arc — the same pointer
/// [`crate::page::Page::backend_page_id`] hashes — so screencast
/// frames, console events, and action `pageId`s all correlate in the
/// viewer.
pub(crate) fn trace_page_id(page: &crate::backend::AnyPage) -> String {
  format!("page@{}", Arc::as_ptr(page.frame_cache()).cast::<()>() as usize)
}

/// Start a screencast on `page` and pump JPEG frames into the trace's
/// film strip. Failure to start (backend without screencast, video
/// recording already holding the stream) degrades to a trace without
/// frames for that page.
pub(crate) async fn spawn_screencast_pump(recorder: &Arc<TraceRecorder>, page: &crate::backend::AnyPage) {
  let Ok((mut rx, stop_tx)) = page.start_screencast(70, 800, 600).await else {
    return;
  };
  recorder.track_screencast_stop(stop_tx);
  let page_id = trace_page_id(page);
  let recorder = Arc::clone(recorder);
  tokio::spawn(async move {
    let mut last_ts = f64::NEG_INFINITY;
    while let Some((jpeg, _backend_ts)) = rx.recv().await {
      let timestamp = recorder.monotonic_ms();
      if timestamp - last_ts < MIN_FRAME_GAP_MS && !recorder.screencast_burst_active(timestamp) {
        continue;
      }
      last_ts = timestamp;
      let (width, height) = jpeg_dimensions(&jpeg).unwrap_or((800, 600));
      // Epoch-ms wall clock: positive and below 2^53, exact as u64.
      #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
      let name = format!("{page_id}-{}.jpeg", recorder.wall_ms() as u64);
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
  before_snapshot: Option<String>,
  after_snapshot: Option<String>,
  stack: Vec<StackFrame>,
}

impl ActionSpan {
  /// The span's `call@N` id — pass as `parent_id` of child spans to
  /// nest them under this action in the viewer.
  #[must_use]
  pub fn call_id(&self) -> &str {
    &self.call_id
  }

  /// Whether the recorder captures DOM snapshots — callers skip the
  /// capture round-trips entirely when off.
  #[must_use]
  pub fn snapshots_enabled(&self) -> bool {
    self.recorder.snapshots
  }

  /// Make this span the live enclosing parent for actions recorded
  /// until [`Self::finish_message_restoring`]; returns the previous
  /// parent to restore.
  #[must_use]
  pub fn make_current_parent(&self) -> Option<String> {
    self.recorder.swap_current_parent(Some(self.call_id.clone()))
  }

  /// Restore the previous enclosing parent, then emit the event.
  pub fn finish_message_restoring(self, error: Option<String>, previous_parent: Option<String>) {
    self.recorder.swap_current_parent(previous_parent);
    self.finish_message(error);
  }

  pub fn set_before_snapshot(&mut self, name: String) {
    self.before_snapshot = Some(name);
  }

  pub fn set_after_snapshot(&mut self, name: String) {
    self.after_snapshot = Some(name);
  }

  /// Emit the action event, recording `error` when the action failed.
  pub fn finish(self, error: Option<&FerriError>) {
    self.finish_message(error.map(std::string::ToString::to_string));
  }

  /// Emit the action event with an already-stringified error (spans
  /// opened by external runners carry plain-text failures).
  pub fn finish_message(self, error: Option<String>) {
    self.recorder.bump_screencast_burst();
    let end_time = self.recorder.monotonic_ms().max(self.start_time);
    self.recorder.push_event(&TraceEvent::Action(ActionEvent {
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
      before_snapshot: self.before_snapshot,
      after_snapshot: self.after_snapshot,
      stack: self.stack,
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
  recorder.bump_screencast_burst();
  let start_time = recorder.monotonic_ms();
  let call_id = recorder.next_call_id();
  let parent_id = recorder.current_parent();
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
    parent_id,
    before_snapshot: None,
    after_snapshot: None,
    stack: Vec::new(),
  })
}

/// A non-protocol action injected into a trace by an external runner
/// (test-runner step boundaries). See [`begin_custom_action`].
pub struct CustomAction {
  /// Trace `class` — the viewer's fallback apiName is `class.method`.
  pub class: &'static str,
  pub method: &'static str,
  /// Display title (wins over `class.method` in the viewer).
  pub title: String,
  pub params: serde_json::Value,
  /// Call id of the enclosing action, for nesting.
  pub parent_id: Option<String>,
  /// Shift the span's start time into the past (spans recorded after
  /// the fact).
  pub backdate_ms: f64,
  /// Call-site stack frames (the viewer's Source tab; a
  /// `sources: true` recording embeds each referenced file).
  pub stack: Vec<StackFrame>,
}

/// Open a titled action span on the active recorder for `composite`.
/// Returns `None` when the composite is not being traced.
#[must_use]
pub fn begin_custom_action(composite: &str, action: CustomAction) -> Option<ActionSpan> {
  let recorder = recorder_for(composite)?;
  recorder.bump_screencast_burst();
  for frame in &action.stack {
    recorder.embed_source(&frame.file);
  }
  let start_time = (recorder.monotonic_ms() - action.backdate_ms).max(0.0);
  let call_id = recorder.next_call_id();
  Some(ActionSpan {
    recorder,
    call_id,
    start_time,
    class: action.class,
    method: action.method.to_string(),
    title: action.title,
    params: action.params,
    page_id: None,
    parent_id: action.parent_id,
    before_snapshot: None,
    after_snapshot: None,
    stack: action.stack,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn context_options_is_first_line_with_version_8() {
    let line = context_options_line("chromium", 1.0, Some("t"));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["version"].as_u64(), Some(8));
    assert_eq!(parsed["type"].as_str(), Some("context-options"));
    assert_eq!(parsed["origin"].as_str(), Some("library"));
    assert_eq!(parsed["title"].as_str(), Some("t"));
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
      before_snapshot: None,
      after_snapshot: None,
      stack: Vec::new(),
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
    let recorder = TraceRecorder::new(&TracingStartOptions::default(), "chromium".into(), 0).expect("spool");
    recorder.push_event(&TraceEvent::Action(ActionEvent {
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
      before_snapshot: None,
      after_snapshot: None,
      stack: Vec::new(),
    }));
    recorder.push_resource(&TraceResource {
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
