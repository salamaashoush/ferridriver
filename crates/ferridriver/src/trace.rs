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
// Each flag is an independent thing to record — screencast frames, DOM
// snapshots, source files, attachment bodies — set one at a time by a
// caller. Grouping them into an enum would be ceremony, not a real state
// machine (the same reading `ContextConfig` carries).
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone)]
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
  /// Embed attachment BODIES as `resources/` entries. The test runner's
  /// `use: { trace: { attachments } }`; Playwright's `tracing.start()`
  /// has no such switch and always embeds, which is why this defaults to
  /// true while `screenshots` / `snapshots` / `sources` default to false.
  pub attachments: bool,
  /// Whether the recording can be read while it is still being made
  /// (Playwright's `live` option).
  pub streaming: TraceStreaming,
}

impl Default for TracingStartOptions {
  fn default() -> Self {
    Self {
      name: None,
      title: None,
      screenshots: false,
      snapshots: false,
      sources: false,
      attachments: true,
      streaming: TraceStreaming::default(),
    }
  }
}

/// When a recording's events reach the file.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum TraceStreaming {
  /// Buffered: nothing is guaranteed on disk until the trace is
  /// exported. What a plain run wants — the trace is read from its zip.
  #[default]
  Buffered,
  /// Flushed per event, so a viewer polling the loose files sees the
  /// recording as it happens. Playwright's `tracing.start({ live: true })`.
  Live,
}

impl TraceStreaming {
  /// From the JS-facing `live?: boolean`.
  #[must_use]
  pub fn from_live(live: bool) -> Self {
    if live { Self::Live } else { Self::Buffered }
  }

  #[must_use]
  pub fn is_live(self) -> bool {
    self == Self::Live
  }
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

/// `file:line` — how a location is written everywhere a person reads one
/// (the `--trace` stream, the `--debug` banner, `pauseAt`'s argument).
/// The column stays a field for the trace, which records all three.
impl std::fmt::Display for StackFrame {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}:{}", self.file, self.line)
  }
}

/// Options bag for `tracing.startChunk`.
///
/// Playwright: `startChunk({ name?, title? })`. `name` renames the
/// recording's stream — a runner gives each test its own name so a live
/// viewer can address it; `title` is what the viewer labels the chunk.
#[derive(Default, Clone)]
pub struct TracingChunkOptions {
  pub name: Option<String>,
  pub title: Option<String>,
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
  /// Action start (`trace.ts` `before` type) — written when the call
  /// begins so live exports show in-flight actions.
  Before(BeforeActionEvent),
  /// Input-time marker (`input` type): input snapshot name + pointer.
  Input(InputActionEvent),
  /// Action end (`after` type): end time, error, attachments.
  After(AfterActionEvent),
  /// One call-log line (`log` type; the viewer's per-action Log pane).
  Log(LogEvent),
  /// Console message (`console` type).
  Console(ConsoleEvent),
  /// Page lifecycle event shown on the timeline (`event` type).
  PageEvent(PageEventEntry),
  /// Test process output (`stdout` / `stderr` types).
  Stdio(StdioEvent),
  /// Screencast frame reference (`screencast-frame` type).
  ScreencastFrame(ScreencastFrameEvent),
  /// DOM snapshot of one frame (`frame-snapshot` type). Carries the
  /// fully built snapshot object (see `crate::snapshotter`).
  FrameSnapshot(serde_json::Value),
  /// A failure that belongs to the run rather than to one call
  /// (`error` type) — an assertion message, a timeout, an unhandled
  /// panic. The viewer's Errors tab is built from these plus the
  /// per-action errors.
  Error(TraceErrorEvent),
}

/// A run-level failure recorded into the trace
/// (`testTracing.ts::appendForError`).
#[derive(Clone)]
pub struct TraceErrorEvent {
  pub message: String,
  pub stack: Vec<StackFrame>,
}

#[derive(Clone)]
pub struct BeforeActionEvent {
  pub call_id: String,
  pub start_time: f64,
  pub class: String,
  pub method: String,
  pub title: String,
  pub params: serde_json::Value,
  pub page_id: Option<String>,
  /// Call id of the enclosing action (nests actions in the viewer's
  /// tree, e.g. test steps under their parent step).
  pub parent_id: Option<String>,
  /// Id of the reporter-visible test step this action belongs to.
  ///
  /// It is how a trace action and a test-runner step are the same thing
  /// to a UI that has both: the viewer keys its step data off it
  /// (`traceModel.hasStepData`), and a runner emitting steps over the
  /// wire uses the same ids. Trace v8 requires it on every action, so a
  /// plain browser call carries its own call id here.
  pub step_id: Option<String>,
  /// `before@<callId>` snapshot name (viewer's Before pane).
  pub before_snapshot: Option<String>,
  /// Call-site stack frames (viewer's Source tab / action location).
  pub stack: Vec<StackFrame>,
}

#[derive(Clone)]
pub struct InputActionEvent {
  pub call_id: String,
  /// `input@<callId>` snapshot name (viewer's Action pane).
  pub input_snapshot: Option<String>,
  /// Viewport point the input was dispatched at (the viewer's red
  /// pointer marker).
  pub point: Option<(f64, f64)>,
}

#[derive(Clone)]
pub struct AfterActionEvent {
  pub call_id: String,
  pub end_time: f64,
  pub error: Option<ActionErrorInfo>,
  /// `after@<callId>` snapshot name (viewer's After pane).
  pub after_snapshot: Option<String>,
  /// Attachments surfaced in the viewer's Attachments tab; bodies are
  /// `resources/<sha1>` entries.
  pub attachments: Vec<TraceAttachment>,
}

/// Serialized action failure (`trace.ts` `SerializedError['error']`).
#[derive(Clone)]
pub struct ActionErrorInfo {
  /// Error class name (`TimeoutError` for deadline failures — the
  /// viewer color-codes it).
  pub name: String,
  pub message: String,
}

impl ActionErrorInfo {
  fn from_ferri(error: &FerriError) -> Self {
    let name = match error {
      FerriError::Timeout { .. } => "TimeoutError",
      _ => "Error",
    };
    Self {
      name: name.to_string(),
      message: error.to_string(),
    }
  }
}

/// One attachment recorded on an action's `after` event.
#[derive(Clone)]
pub struct TraceAttachment {
  pub name: String,
  pub content_type: String,
  /// Resource entry name (`resources/<sha1>.<ext>`).
  pub sha1: String,
}

#[derive(Clone)]
pub struct LogEvent {
  pub call_id: String,
  pub time: f64,
  pub message: String,
}

#[derive(Clone)]
pub struct StdioEvent {
  /// `stdout` or `stderr`.
  pub kind: &'static str,
  pub timestamp: f64,
  pub text: String,
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

/// Where a recording's loose files live, and who is responsible for
/// removing them.
#[derive(Clone, Debug)]
pub struct TraceLocation {
  /// Directory holding `<name>.trace`, `<name>.network` and `resources/`.
  pub dir: std::path::PathBuf,
  /// Stream name — Playwright's `traceName`. A live viewer finds a
  /// recording by asking for `<name>.json`, so a runner names its traces
  /// after the test id.
  pub name: String,
  /// Whether the recorder owns `dir` and deletes it when the recording
  /// ends. False when a caller supplied `tracesDir`: those files are the
  /// caller's (the live viewer is still reading them).
  pub owned: bool,
}

impl TraceLocation {
  /// A private directory under the system temp dir, removed when the
  /// recording ends.
  #[must_use]
  pub fn temporary() -> Self {
    static NEXT_SPOOL_ID: AtomicU64 = AtomicU64::new(1);
    Self {
      dir: std::env::temp_dir().join(format!(
        "ferridriver-trace-{}-{}",
        std::process::id(),
        NEXT_SPOOL_ID.fetch_add(1, Ordering::Relaxed)
      )),
      name: "trace".to_string(),
      owned: true,
    }
  }

  /// Files under a caller-supplied `tracesDir`, left in place afterwards.
  #[must_use]
  pub fn in_dir(dir: std::path::PathBuf, name: String) -> Self {
    Self {
      dir,
      name,
      owned: false,
    }
  }

  fn trace_file(&self) -> std::path::PathBuf {
    self.dir.join(format!("{}.trace", self.name))
  }

  fn network_file(&self) -> std::path::PathBuf {
    self.dir.join(format!("{}.network", self.name))
  }
}

/// On-disk spool for an in-flight recording: events append to a
/// `<name>.trace` JSONL file, resources land under `resources/` as they
/// arrive. Memory stays flat no matter how long the recording runs
/// (screencast frames alone would otherwise grow unbounded); export
/// streams the spool into the final zip.
///
/// A live recording is read straight out of this directory by the trace
/// viewer — that is what `live` buys: every line is flushed as it is
/// written, so a poll never sees a half-written event.
struct TraceSpool {
  location: TraceLocation,
  trace: std::io::BufWriter<std::fs::File>,
  streaming: TraceStreaming,
  /// sha1-style resource names already written (dedup).
  written_resources: rustc_hash::FxHashSet<String>,
}

impl TraceSpool {
  fn create(location: TraceLocation, streaming: TraceStreaming, first_line: &str) -> Result<Self> {
    std::fs::create_dir_all(location.dir.join("resources"))
      .map_err(|e| FerriError::backend(format!("create trace spool {}: {e}", location.dir.display())))?;
    let file = std::fs::File::create(location.trace_file())
      .map_err(|e| FerriError::backend(format!("create trace spool file: {e}")))?;
    // A live recording has a reader (the viewer's descriptor poll) that
    // must never be handed a truncated last line.
    let mut spool = Self {
      location,
      trace: std::io::BufWriter::new(file),
      streaming,
      written_resources: rustc_hash::FxHashSet::default(),
    };
    spool.write_line(first_line);
    Ok(spool)
  }

  fn write_line(&mut self, line: &str) {
    use std::io::Write;
    let _ = self.trace.write_all(line.as_bytes());
    let _ = self.trace.write_all(b"\n");
    if self.streaming.is_live() {
      let _ = self.trace.flush();
    }
  }

  fn write_resource(&mut self, resource: &TraceResource) {
    if !self.written_resources.insert(resource.name.clone()) {
      return;
    }
    let _ = std::fs::write(
      self.location.dir.join("resources").join(&resource.name),
      &resource.bytes,
    );
  }
}

impl Drop for TraceSpool {
  fn drop(&mut self) {
    if self.location.owned {
      let _ = std::fs::remove_dir_all(&self.location.dir);
    }
  }
}

/// Live trace recorder, stored per-context on
/// [`crate::state::BrowserState`] between `tracing.start` and
/// `tracing.stop`. All interior mutability is sync — the action hot
/// path appends a serialized line to the disk spool under a brief
/// mutex.
#[allow(clippy::struct_excessive_bools)]
pub struct TraceRecorder {
  /// Monotonic origin: event times are milliseconds since this instant.
  origin: Instant,
  /// Wall-clock anchor paired with `origin` (epoch ms).
  wall_origin: f64,
  /// Trace title (`context-options.title`). Per-chunk: `startChunk`
  /// relabels the recording without restarting it, which is how a test
  /// runner titles each test's chunk of one long-lived recording.
  title: std::sync::Mutex<Option<String>>,
  /// Whether screencast frames are being captured.
  pub screenshots: bool,
  /// Whether DOM snapshots are being captured around actions.
  pub snapshots: bool,
  /// Whether source files referenced by action stacks are embedded.
  pub sources: bool,
  /// Whether attachment bodies are embedded as resources.
  pub attachments: bool,
  /// Whether every event is flushed as it is written, for a reader
  /// watching the recording as it happens.
  streaming: TraceStreaming,
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
  /// Open `tracing.group()` calls, innermost last. A group is an action
  /// like any other in the trace — the stack is what makes everything
  /// recorded until `groupEnd()` a child of it.
  group_stack: std::sync::Mutex<Vec<String>>,
  /// Shutdown senders for per-page screencast pumps.
  screencast_stops: std::sync::Mutex<Vec<tokio::sync::oneshot::Sender<()>>>,
  /// Browser name recorded in `context-options`.
  browser_name: String,
  /// Context-creation options recorded in `context-options.options`
  /// (viewport etc. — the viewer's Metadata tab).
  context_options: serde_json::Value,
  /// Bumped on every appended event/resource ([`Self::spool_version`]).
  spool_version: AtomicU64,
  /// Snapshot-history epoch for this chunk: page documents compare it
  /// against their stored value at capture time and self-reset on
  /// mismatch ([`crate::snapshotter`]). Process-unique so a document
  /// that outlived a previous recording (or chunk) can never reuse its
  /// node-dedup cache against the new file.
  snapshot_epoch: AtomicU64,
}

/// Process-global source for [`TraceRecorder::snapshot_epoch`] values.
static NEXT_SNAPSHOT_EPOCH: AtomicU64 = AtomicU64::new(1);

impl TraceRecorder {
  /// Start recording into `location`.
  ///
  /// # Errors
  ///
  /// Errors if the on-disk spool cannot be created.
  pub fn new(
    options: &TracingStartOptions,
    browser_name: String,
    context_options: serde_json::Value,
    network_len: usize,
    location: TraceLocation,
  ) -> Result<Self> {
    let origin = Instant::now();
    let wall_origin = now_epoch_ms();
    let first_line = context_options_line(
      &browser_name,
      wall_origin,
      0.0,
      options.title.as_deref(),
      &context_options,
    );
    Ok(Self {
      origin,
      wall_origin,
      title: std::sync::Mutex::new(options.title.clone()),
      screenshots: options.screenshots,
      snapshots: options.snapshots,
      sources: options.sources,
      attachments: options.attachments,
      streaming: options.streaming,
      screencast_burst_until_ms: AtomicU64::new(0),
      sources_embedded: std::sync::Mutex::new(rustc_hash::FxHashSet::default()),
      spool: std::sync::Mutex::new(TraceSpool::create(location, options.streaming, &first_line)?),
      network_start_len: AtomicU64::new(network_len as u64),
      next_call_id: AtomicU64::new(1),
      current_parent: std::sync::Mutex::new(None),
      group_stack: std::sync::Mutex::new(Vec::new()),
      screencast_stops: std::sync::Mutex::new(Vec::new()),
      browser_name,
      context_options,
      spool_version: AtomicU64::new(0),
      snapshot_epoch: AtomicU64::new(NEXT_SNAPSHOT_EPOCH.fetch_add(1, Ordering::Relaxed)),
    })
  }

  /// Title of the current chunk, as the viewer shows it.
  #[must_use]
  pub fn title(&self) -> Option<String> {
    self
      .title
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  /// Where this recording is being written, and under what name — what a
  /// runner hands a viewer so it can follow the trace as it grows.
  #[must_use]
  pub fn location(&self) -> TraceLocation {
    self
      .spool
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .location
      .clone()
  }

  /// Current snapshot-history epoch (see the field doc).
  #[must_use]
  pub(crate) fn snapshot_epoch(&self) -> u64 {
    self.snapshot_epoch.load(Ordering::Relaxed)
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

  /// What a newly recorded action nests under: the live span when one is
  /// open (a test step), otherwise the innermost `tracing.group()`.
  fn current_parent(&self) -> Option<String> {
    let span = self
      .current_parent
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    span.or_else(|| self.current_group())
  }

  fn current_group(&self) -> Option<String> {
    self
      .group_stack
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .last()
      .cloned()
  }

  /// Open a `tracing.group()`: a titled action everything recorded until
  /// [`Self::end_group`] nests under (`tracing.ts::group`).
  pub fn begin_group(&self, name: String, stack: Vec<StackFrame>) {
    for frame in &stack {
      self.embed_source(&frame.file);
    }
    let call_id = self.next_call_id();
    let parent_id = self.current_parent();
    self.push_event(&TraceEvent::Before(BeforeActionEvent {
      call_id: call_id.clone(),
      start_time: self.monotonic_ms(),
      class: "Tracing".to_string(),
      method: "tracingGroup".to_string(),
      title: name,
      params: serde_json::json!({}),
      page_id: None,
      parent_id,
      step_id: Some(call_id.clone()),
      before_snapshot: None,
      stack,
    }));
    self
      .group_stack
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(call_id);
  }

  /// Close the innermost open group. A no-op when none is open, matching
  /// `tracing.ts::_groupEnd`.
  pub fn end_group(&self) {
    let call_id = self
      .group_stack
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .pop();
    let Some(call_id) = call_id else { return };
    self.push_event(&TraceEvent::After(AfterActionEvent {
      call_id,
      end_time: self.monotonic_ms(),
      error: None,
      after_snapshot: None,
      attachments: Vec::new(),
    }));
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
    self.spool_version.fetch_add(1, Ordering::Relaxed);
  }

  pub fn push_resource(&self, resource: &TraceResource) {
    self
      .spool
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .write_resource(resource);
    self.spool_version.fetch_add(1, Ordering::Relaxed);
  }

  /// Monotonic counter bumped on every appended event/resource — live
  /// exporters skip re-zipping when nothing changed since their last
  /// snapshot.
  #[must_use]
  pub fn spool_version(&self) -> u64 {
    self.spool_version.load(Ordering::Relaxed)
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
  /// old spool is replaced (and its directory removed on drop, when the
  /// recorder owns it). The fresh `context-options` line carries the
  /// CURRENT monotonic time — the chunk's events start there, not at 0
  /// (`tracing.ts` stamps `monotonicTime()` per chunk; a 0 would show a
  /// dead lead-in on the viewer timeline).
  ///
  /// `name` renames the stream for the new chunk (Playwright's
  /// `startChunk({ name })`), keeping the same directory; `title`
  /// relabels it in the viewer.
  pub fn start_chunk(&self, network_len: usize, name: Option<String>, title: Option<String>) {
    if let Some(title) = title {
      *self.title.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(title);
    }
    let first_line = context_options_line(
      &self.browser_name,
      self.wall_origin,
      self.monotonic_ms(),
      self.title().as_deref(),
      &self.context_options,
    );
    let mut location = self.location();
    if let Some(name) = name {
      location.name = name;
    }
    if let Ok(fresh) = TraceSpool::create(location, self.streaming, &first_line) {
      let mut guard = self.spool.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      *guard = fresh;
    }
    self.network_start_len.store(network_len as u64, Ordering::SeqCst);
    // Fresh epoch: `[[n,m]]` back-references into the previous chunk's
    // snapshots would dangle, so every document self-resets on its next
    // capture.
    self
      .snapshot_epoch
      .store(NEXT_SNAPSHOT_EPOCH.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
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

  /// Freeze the spool's current extent under the lock: flush the trace
  /// writer, record the flushed byte length, and list the fully written
  /// resource names. Zipping happens AFTER the lock is released —
  /// `push_event` sits on the action hot path and must never wait out a
  /// multi-megabyte deflate (live exports run every poll tick).
  fn snapshot_spool(&self) -> Result<SpoolSnapshot> {
    let mut spool = self.spool.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    std::io::Write::flush(&mut spool.trace).map_err(|e| FerriError::backend(format!("flush trace spool: {e}")))?;
    let trace_len = spool
      .trace
      .get_ref()
      .metadata()
      .map_err(|e| FerriError::backend(format!("stat trace spool: {e}")))?
      .len();
    Ok(SpoolSnapshot {
      trace_file: spool.location.trace_file(),
      network_file: spool.location.network_file(),
      resources_dir: spool.location.dir.join("resources"),
      trace_len,
      resources: spool.written_resources.iter().cloned().collect(),
    })
  }

  /// Stream the spooled chunk into a Playwright-compatible `trace.zip`
  /// at `path`. Memory stays flat — the spool files are copied into the
  /// archive, never loaded whole. The spool lock is held only long
  /// enough to freeze the export extent; concurrent appends past that
  /// point land in the next export.
  ///
  /// # Errors
  ///
  /// Errors if serialization or the zip write fails.
  pub fn export(&self, path: &std::path::Path, network_entries: &[serde_json::Value]) -> Result<()> {
    // Written beside the trace first: a recording left on disk (a live
    // directory, a run that is inspected before its zip is opened) is
    // then a complete trace on its own, network tab included.
    self.persist_network(network_entries);
    let snapshot = self.snapshot_spool()?;
    let file = std::fs::File::create(path)
      .map_err(|e| FerriError::backend(format!("create trace zip {}: {e}", path.display())))?;
    write_trace_zip(&snapshot, file, network_entries)
  }

  /// Write the chunk's network stream to `<name>.network`.
  fn persist_network(&self, entries: &[serde_json::Value]) {
    let path = self.location().network_file();
    let mut body = String::new();
    for entry in entries {
      use std::fmt::Write as _;
      let _ = writeln!(
        body,
        "{}",
        serde_json::json!({ "type": "resource-snapshot", "snapshot": entry })
      );
    }
    let _ = std::fs::write(path, body);
  }

  /// [`Self::export`] into an in-memory buffer — the live-trace endpoint
  /// serves the bytes straight from RAM instead of a temp-file round
  /// trip per poll.
  ///
  /// # Errors
  ///
  /// Errors if serialization or the zip write fails.
  pub fn export_to_vec(&self, network_entries: &[serde_json::Value]) -> Result<Vec<u8>> {
    let snapshot = self.snapshot_spool()?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    write_trace_zip(&snapshot, &mut cursor, network_entries)?;
    Ok(cursor.into_inner())
  }
}

/// Frozen extent of a spool at export time (see
/// [`TraceRecorder::snapshot_spool`]).
struct SpoolSnapshot {
  trace_file: std::path::PathBuf,
  network_file: std::path::PathBuf,
  resources_dir: std::path::PathBuf,
  trace_len: u64,
  resources: Vec<String>,
}

/// Whether a resource is already compressed — deflating JPEG frames,
/// PNGs, or nested zips burns CPU on every live poll for ~0% gain, so
/// those are stored raw.
fn resource_is_precompressed(name: &str) -> bool {
  std::path::Path::new(name)
    .extension()
    .and_then(|e| e.to_str())
    .is_some_and(|ext| matches!(ext, "jpeg" | "jpg" | "png" | "webp" | "zip" | "webm"))
}

fn write_trace_zip<W: std::io::Write + std::io::Seek>(
  snapshot: &SpoolSnapshot,
  writer: W,
  network_entries: &[serde_json::Value],
) -> Result<()> {
  use std::io::Write;

  let mut writer = zip::ZipWriter::new(writer);
  let deflated = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
  let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
  let zip_err = |e: zip::result::ZipError| FerriError::backend(format!("write trace zip: {e}"));
  let io_err = |e: std::io::Error| FerriError::backend(format!("write trace zip: {e}"));

  // Canonical entry names inside the archive, whatever the loose files on
  // disk were called: a single-context zip is `trace.trace` +
  // `trace.network` (`tracing.ts::_exportZip`).
  writer.start_file("trace.trace", deflated).map_err(zip_err)?;
  let trace_file =
    std::fs::File::open(&snapshot.trace_file).map_err(|e| FerriError::backend(format!("open trace spool: {e}")))?;
  // Copy only the frozen extent — the recorder may have appended since.
  let mut trace_file = std::io::Read::take(trace_file, snapshot.trace_len);
  std::io::copy(&mut trace_file, &mut writer).map_err(io_err)?;

  writer.start_file("trace.network", deflated).map_err(zip_err)?;
  // The finished network stream is on disk next to the trace; a live
  // snapshot has none yet and serializes what it was handed (nothing).
  if let Ok(mut network) = std::fs::File::open(&snapshot.network_file) {
    std::io::copy(&mut network, &mut writer).map_err(io_err)?;
  } else {
    for entry in network_entries {
      let wrapped = serde_json::json!({ "type": "resource-snapshot", "snapshot": entry });
      writer.write_all(wrapped.to_string().as_bytes()).map_err(io_err)?;
      writer.write_all(b"\n").map_err(io_err)?;
    }
  }

  let resources_dir = &snapshot.resources_dir;
  for name in &snapshot.resources {
    // A resource can vanish under a concurrent `start_chunk` swap (the
    // old spool dir is removed); a live snapshot just skips it.
    let Ok(mut resource) = std::fs::File::open(resources_dir.join(name)) else {
      continue;
    };
    let opts = if resource_is_precompressed(name) {
      stored
    } else {
      deflated
    };
    writer.start_file(format!("resources/{name}"), opts).map_err(zip_err)?;
    std::io::copy(&mut resource, &mut writer).map_err(io_err)?;
  }

  writer.finish().map_err(zip_err)?;
  Ok(())
}

/// Remove trace spool directories left behind by dead processes. Spool
/// dirs are named `ferridriver-trace-<pid>-<n>`; a run killed with
/// SIGKILL (UI
/// Stop kills the cycle's process group) never runs [`TraceSpool`]'s
/// `Drop`, so long-lived UI servers sweep on startup. Live processes'
/// spools (including this one's) are left alone.
pub fn sweep_stale_spools() {
  let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
    return;
  };
  let own_pid = std::process::id();
  for entry in entries.flatten() {
    let name = entry.file_name();
    let Some(name) = name.to_str() else { continue };
    let Some(rest) = name.strip_prefix("ferridriver-trace-") else {
      continue;
    };
    let Some((pid, _)) = rest.split_once('-') else { continue };
    let Ok(pid) = pid.parse::<u32>() else { continue };
    if pid == own_pid || process_alive(pid) {
      continue;
    }
    let _ = std::fs::remove_dir_all(entry.path());
  }
}

fn process_alive(pid: u32) -> bool {
  let Ok(pid) = i32::try_from(pid) else {
    return false;
  };
  // SAFETY: kill(2) with signal 0 touches no memory; it only probes for
  // process existence (0 = alive, EPERM = alive but not ours).
  #[allow(unsafe_code)]
  let rc = unsafe { libc::kill(pid, 0) };
  rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Platform string in the form the viewer's metadata pane (and every
/// Playwright trace ever recorded) uses: node's `process.platform`, not
/// Rust's `std::env::consts::OS`.
fn trace_platform() -> &'static str {
  match std::env::consts::OS {
    "macos" => "darwin",
    "windows" => "win32",
    other => other,
  }
}

/// What produced the trace, shown in the viewer's metadata pane where
/// Playwright puts its own version.
fn recorder_version() -> String {
  format!("ferridriver/{}", env!("CARGO_PKG_VERSION"))
}

/// First trace line: `context-options` with `version: 8` (the loader
/// mis-modernizes everything as v6 without it). `monotonic` anchors
/// the chunk's start on the timeline (0 for a fresh recording, the
/// current clock for later chunks).
fn context_options_line(
  browser_name: &str,
  wall_origin: f64,
  monotonic: f64,
  title: Option<&str>,
  context_options: &serde_json::Value,
) -> String {
  serde_json::json!({
    "version": TRACE_VERSION,
    "type": "context-options",
    "origin": "library",
    "browserName": browser_name,
    "platform": trace_platform(),
    "playwrightVersion": recorder_version(),
    "wallTime": wall_origin + monotonic,
    "monotonicTime": monotonic,
    "title": title.unwrap_or_default(),
    "options": context_options,
    "sdkLanguage": "javascript",
    // The viewer resolves a recorded `getByTestId` back to source with
    // this, so it has to be the attribute the run actually used.
    "testIdAttributeName": crate::selectors::default_test_id_attribute(),
  })
  .to_string()
}

/// Insert `key` only when the value is present — Playwright's writers
/// omit absent optionals rather than writing `null`.
fn insert_opt(obj: &mut serde_json::Map<String, serde_json::Value>, key: &str, value: Option<serde_json::Value>) {
  if let Some(value) = value {
    obj.insert(key.to_string(), value);
  }
}

fn serialize_before(b: &BeforeActionEvent) -> String {
  let mut obj = serde_json::Map::new();
  obj.insert("type".into(), "before".into());
  obj.insert("callId".into(), b.call_id.clone().into());
  obj.insert("startTime".into(), b.start_time.into());
  obj.insert("class".into(), b.class.clone().into());
  obj.insert("method".into(), b.method.clone().into());
  obj.insert("title".into(), b.title.clone().into());
  obj.insert("params".into(), b.params.clone());
  insert_opt(&mut obj, "pageId", b.page_id.clone().map(Into::into));
  insert_opt(&mut obj, "parentId", b.parent_id.clone().map(Into::into));
  insert_opt(&mut obj, "stepId", b.step_id.clone().map(Into::into));
  insert_opt(&mut obj, "beforeSnapshot", b.before_snapshot.clone().map(Into::into));
  if !b.stack.is_empty() {
    obj.insert(
      "stack".into(),
      b.stack
        .iter()
        .map(|f| serde_json::json!({ "file": f.file, "line": f.line, "column": f.column }))
        .collect::<Vec<_>>()
        .into(),
    );
  }
  serde_json::Value::Object(obj).to_string()
}

fn serialize_input(i: &InputActionEvent) -> String {
  let mut obj = serde_json::Map::new();
  obj.insert("type".into(), "input".into());
  obj.insert("callId".into(), i.call_id.clone().into());
  insert_opt(&mut obj, "inputSnapshot", i.input_snapshot.clone().map(Into::into));
  insert_opt(
    &mut obj,
    "point",
    i.point.map(|(x, y)| serde_json::json!({ "x": x, "y": y })),
  );
  serde_json::Value::Object(obj).to_string()
}

fn serialize_after(a: &AfterActionEvent) -> String {
  let mut obj = serde_json::Map::new();
  obj.insert("type".into(), "after".into());
  obj.insert("callId".into(), a.call_id.clone().into());
  obj.insert("endTime".into(), a.end_time.into());
  insert_opt(
    &mut obj,
    "error",
    a.error
      .as_ref()
      .map(|e| serde_json::json!({ "name": e.name, "message": e.message })),
  );
  insert_opt(&mut obj, "afterSnapshot", a.after_snapshot.clone().map(Into::into));
  if !a.attachments.is_empty() {
    obj.insert(
      "attachments".into(),
      a.attachments
        .iter()
        .map(|att| serde_json::json!({ "name": att.name, "contentType": att.content_type, "sha1": att.sha1 }))
        .collect::<Vec<_>>()
        .into(),
    );
  }
  serde_json::Value::Object(obj).to_string()
}

fn serialize_event(event: &TraceEvent) -> String {
  match event {
    TraceEvent::Before(b) => serialize_before(b),
    TraceEvent::Input(i) => serialize_input(i),
    TraceEvent::After(a) => serialize_after(a),
    TraceEvent::Log(l) => serde_json::json!({
      "type": "log",
      "callId": l.call_id,
      "time": l.time,
      "message": l.message,
    })
    .to_string(),
    TraceEvent::Stdio(s) => serde_json::json!({
      "type": s.kind,
      "timestamp": s.timestamp,
      "text": s.text,
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
    TraceEvent::Error(error) => serde_json::json!({
      "type": "error",
      "message": error.message,
      "stack": error
        .stack
        .iter()
        .map(|frame| serde_json::json!({ "file": frame.file, "line": frame.line, "column": frame.column }))
        .collect::<Vec<_>>(),
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

/// True while any composite is being recorded. Lets [`call_origins_wanted`]
/// answer without taking the recorder lock — it is asked once per API call
/// from the host language, including in processes that never trace.
static RECORDING_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
  // A just-failed main-frame navigation leaves the CDP session
  // transiently "Not attached to an active page"; the state self-heals
  // within milliseconds, so retry briefly instead of silently
  // recording a frameless trace.
  let mut attempt = 0;
  let (mut rx, stop_tx) = loop {
    match page.start_screencast(70, 800, 600).await {
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

// ── Action spans ───────────────────────────────────────────────────────

/// Identity of a traced action, handed to an [`ActionObserver`].
#[derive(Clone)]
pub struct ActionInfo {
  /// `call@N` id, unique within the process.
  pub call_id: String,
  /// API class (`Page`, `Locator`, `Expect`, ...).
  pub class: String,
  /// Method name (`goto`, `click`, `toBeVisible`, ...).
  pub method: String,
  /// Display title (`page.goto`).
  pub title: String,
  /// Call parameters as recorded in the trace.
  pub params: serde_json::Value,
  /// Where the call was written, when the host captured a call site.
  pub location: Option<StackFrame>,
  /// Which script issued the call — see [`CallOrigin::script`].
  pub script: Option<Arc<str>>,
}

// ── Call origin ────────────────────────────────────────────────────────

/// What a host knows about an API call that core cannot work out for
/// itself: where it was written, and who wrote it.
#[derive(Clone, Default)]
pub struct CallOrigin {
  /// Source position of the call, already mapped back to the file the
  /// user wrote (not the bundle the engine ran).
  pub location: Option<StackFrame>,
  /// Identity of the script that issued the call.
  ///
  /// A paused test and the client inspecting it drive the same browser
  /// through the same context, so a gate cannot tell them apart by
  /// anything the action itself carries. Pausing the inspecting client
  /// would deadlock it — nobody is left to resume — so the gate skips
  /// calls it recognises as its own.
  pub script: Option<Arc<str>>,
}

tokio::task_local! {
  /// Set while an API action is open on this task.
  ///
  /// A public method that delegates to another public method
  /// (`page.setContent` waiting through `waitForLoadState`,
  /// `page.evaluate` going through the main frame) must record ONE
  /// action, not a nest of them — Playwright suppresses the inner calls
  /// the same way, by noticing its api zone is already entered.
  static IN_ACTION: ();
}

/// Run `fut` as the body of an open API action: actions opened inside it
/// are the SAME call and record nothing of their own.
pub(crate) async fn within_action<F: std::future::Future>(fut: F) -> F::Output {
  if IN_ACTION.try_with(|()| ()).is_ok() {
    return fut.await;
  }
  IN_ACTION.scope((), fut).await
}

/// Whether an API action is already open on this task.
fn action_in_progress() -> bool {
  IN_ACTION.try_with(|()| ()).is_ok()
}

tokio::task_local! {
  /// Origin of the API call the current future is performing.
  ///
  /// The host scopes this at the language boundary, where the caller's
  /// stack is still live; core reads it several awaits later, inside
  /// [`begin_action`]. A task-local and not a slot because
  /// `Promise.all([a.click(), b.click()])` puts two call sites in flight
  /// at once, and each future has to keep its own.
  static CALL_ORIGIN: CallOrigin;
}

/// Run `fut` with `origin` as the call origin of the actions it opens.
///
/// Not an `async fn`: that would be a state machine holding `fut` in more
/// than one state, which doubles the size of every action future it wraps —
/// and browser-action futures are already big enough to trip
/// `clippy::large_futures` on their own.
pub fn with_call_origin<F: std::future::Future>(
  origin: CallOrigin,
  fut: F,
) -> impl std::future::Future<Output = F::Output> {
  CALL_ORIGIN.scope(origin, fut)
}

/// Whether anything would read a call origin.
///
/// Capturing one costs the host a stack walk per call (`new Error().stack`
/// in the script engine), so hosts ask before paying for it.
#[must_use]
pub fn call_origins_wanted() -> bool {
  RECORDING_ACTIVE.load(Ordering::Acquire)
    || ACTION_GATE_INSTALLED.load(Ordering::Acquire)
    || ACTION_OBSERVER_INSTALLED.load(Ordering::Acquire)
}

fn current_call_origin() -> CallOrigin {
  CALL_ORIGIN.try_with(Clone::clone).unwrap_or_default()
}

/// The Rust call site of whoever called this.
///
/// The script engine reads a JS stack; a Rust test has `#[track_caller]`,
/// which is exact and free. Chains through any `#[track_caller]` caller, so
/// a builder method marked with it reports the line the user wrote rather
/// than its own body.
///
/// Yields nothing when an origin is already in scope. The host that set it
/// knows the caller's real language: under a script, the Rust builder's
/// `#[track_caller]` site is a file inside `ferridriver-script`, and
/// reporting that instead of the user's `.ts` line is worse than reporting
/// nothing.
#[must_use]
#[track_caller]
pub fn call_origin_here() -> CallOrigin {
  if !call_origins_wanted() || CALL_ORIGIN.try_with(|_| ()).is_ok() {
    return CallOrigin::default();
  }
  let caller = std::panic::Location::caller();
  CallOrigin {
    location: Some(StackFrame {
      file: caller.file().to_string(),
      line: caller.line(),
      column: caller.column(),
    }),
    script: None,
  }
}

// ── Action gate ────────────────────────────────────────────────────────

/// A pause point in front of every action.
///
/// `ferridriver test --debug` installs one to hold a test between its API
/// calls. An [`ActionObserver`] only watches; a gate decides when the
/// action gets to run, which is why it is async and why it is a separate
/// trait rather than another observer method.
#[async_trait::async_trait]
pub trait ActionGate: Send + Sync + 'static {
  /// Called with the action about to run. Returning is what releases it.
  async fn before_action(&self, action: &ActionInfo);
}

static ACTION_GATE: std::sync::RwLock<Option<Arc<dyn ActionGate>>> = std::sync::RwLock::new(None);

/// Fast path: an ungated process pays one relaxed load per action rather
/// than a lock acquisition.
static ACTION_GATE_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Install the process's action gate, replacing any previous one.
pub fn set_action_gate(gate: Arc<dyn ActionGate>) {
  *ACTION_GATE.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(gate);
  ACTION_GATE_INSTALLED.store(true, Ordering::Release);
}

/// Remove the action gate. Actions already blocked on it are the gate's
/// own problem to release — clearing it only stops new ones from waiting.
pub fn clear_action_gate() {
  *ACTION_GATE.write().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
  ACTION_GATE_INSTALLED.store(false, Ordering::Release);
}

fn action_gate() -> Option<Arc<dyn ActionGate>> {
  if !ACTION_GATE_INSTALLED.load(Ordering::Acquire) {
    return None;
  }
  ACTION_GATE
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
}

/// Live view of the action stream, independent of trace recording.
///
/// `ferridriver run --trace` installs one to print each browser action as it
/// starts and finishes; unlike [`TraceRecorder`] it needs no
/// `context.tracing.start()` and writes no zip.
pub trait ActionObserver: Send + Sync + 'static {
  fn action_begin(&self, action: &ActionInfo);
  fn action_end(&self, action: &ActionInfo, elapsed: std::time::Duration, error: Option<&str>);
  /// One call-log line (`waiting for locator(...)`) while the action runs.
  fn action_log(&self, action: &ActionInfo, message: &str);
}

/// Set once by the host before any script runs; every later probe is the
/// atomic below, so an unobserved process pays a relaxed load per action.
static ACTION_OBSERVER: std::sync::RwLock<Option<Arc<dyn ActionObserver>>> = std::sync::RwLock::new(None);

/// Per-session observers, keyed by composite session key.
///
/// A process that hosts several sessions at once (a bound browser serving
/// attached clients) needs each client to see ITS actions and no one else's.
/// Actions already carry the composite they belong to, so scoping is a lookup
/// rather than any new plumbing through the call sites.
static SESSION_ACTION_OBSERVERS: std::sync::RwLock<Option<rustc_hash::FxHashMap<String, Arc<dyn ActionObserver>>>> =
  std::sync::RwLock::new(None);

/// True when a global or any session observer exists. Keeps the unobserved
/// hot path at one relaxed load rather than two lock acquisitions.
static ACTION_OBSERVER_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Install the process-wide action observer, replacing any previous one.
/// A session-scoped observer wins over this one for that session's actions.
pub fn set_action_observer(observer: Arc<dyn ActionObserver>) {
  *ACTION_OBSERVER
    .write()
    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observer);
  ACTION_OBSERVER_INSTALLED.store(true, Ordering::Release);
}

/// Observe only the actions of session `composite`, until the returned guard
/// drops.
///
/// Replaces any observer already scoped to that session — a session runs one
/// script at a time, so two live observers on one key would mean a bug, not a
/// second audience.
#[must_use]
pub fn observe_session_actions(composite: &str, observer: Arc<dyn ActionObserver>) -> SessionObserverGuard {
  {
    let mut guard = SESSION_ACTION_OBSERVERS
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
      .get_or_insert_with(rustc_hash::FxHashMap::default)
      .insert(composite.to_string(), observer);
  }
  ACTION_OBSERVER_INSTALLED.store(true, Ordering::Release);
  SessionObserverGuard {
    composite: composite.to_string(),
  }
}

/// Removes its session's action observer on drop.
pub struct SessionObserverGuard {
  composite: String,
}

impl Drop for SessionObserverGuard {
  fn drop(&mut self) {
    let mut guard = SESSION_ACTION_OBSERVERS
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(map) = guard.as_mut() {
      map.remove(&self.composite);
      if map.is_empty() {
        *guard = None;
        // Only clear the fast-path flag when no global observer remains
        // either, or a `run --trace` in the same process would go silent.
        let global_present = ACTION_OBSERVER
          .read()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .is_some();
        if !global_present {
          ACTION_OBSERVER_INSTALLED.store(false, Ordering::Release);
        }
      }
    }
  }
}

fn action_observer(composite: Option<&str>) -> Option<Arc<dyn ActionObserver>> {
  if !ACTION_OBSERVER_INSTALLED.load(Ordering::Acquire) {
    return None;
  }
  if let Some(composite) = composite {
    let scoped = SESSION_ACTION_OBSERVERS
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(observer) = scoped.as_ref().and_then(|map| map.get(composite)) {
      return Some(Arc::clone(observer));
    }
  }
  ACTION_OBSERVER
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .clone()
}

/// An observed action's start state, carried by the span until it closes.
struct ObservedAction {
  observer: Arc<dyn ActionObserver>,
  started: Instant,
}

/// An in-flight traced action. [`begin_action`] /
/// [`begin_custom_action`] write the `before` event immediately (live
/// exports show the action while it runs); [`ActionSpan::finish`]
/// writes the `after` event. Snapshot names are decided up front —
/// exactly like Playwright, where the `before` line references
/// `before@<callId>` before the async capture lands.
///
/// A span also exists with no recorder at all when only an
/// [`ActionObserver`] is installed: every recorder-bound method then no-ops
/// and the span exists purely to report the action's start and outcome.
pub struct ActionSpan {
  recorder: Option<Arc<TraceRecorder>>,
  /// Built once when anything is watching (an observer, a gate, or both)
  /// and shared by them, so an action that is watched twice is still
  /// described once.
  info: Option<Arc<ActionInfo>>,
  /// Boxed so an unobserved span — every span in a normal run — stays small
  /// enough not to bloat the futures that hold one across an await.
  observed: Option<Box<ObservedAction>>,
  /// Holds the action until the gate lets it run ([`ActionSpan::open`]).
  gate: Option<Arc<dyn ActionGate>>,
  call_id: String,
  /// `before@<callId>` when the recorder captures snapshots and the
  /// action is page-bound.
  before_snapshot: Option<String>,
  after_snapshot: Option<String>,
  attachments: Vec<TraceAttachment>,
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
    self.recorder.as_ref().is_some_and(|r| r.snapshots)
  }

  /// Snapshot name the `before` event referenced (`None` when the
  /// recorder is not capturing snapshots).
  #[must_use]
  pub fn before_snapshot_name(&self) -> Option<&str> {
    self.before_snapshot.as_deref()
  }

  /// Snapshot name the `after` event will reference; marks it so
  /// `finish` includes it.
  pub fn set_after_snapshot(&mut self, name: String) {
    self.after_snapshot = Some(name);
  }

  /// Make this span the live enclosing parent for actions recorded
  /// until [`Self::finish_message_restoring`]; returns the previous
  /// parent to restore.
  #[must_use]
  pub fn make_current_parent(&self) -> Option<String> {
    self
      .recorder
      .as_ref()
      .and_then(|r| r.swap_current_parent(Some(self.call_id.clone())))
  }

  /// Restore the previous enclosing parent, then emit the event.
  pub fn finish_message_restoring(self, error: Option<String>, previous_parent: Option<String>) {
    if let Some(recorder) = self.recorder.as_ref() {
      recorder.swap_current_parent(previous_parent);
    }
    self.finish_message(error);
  }

  /// Append one line to this action's call log (the viewer's Log pane).
  pub fn log(&self, message: impl Into<String>) {
    let message = message.into();
    if let (Some(observed), Some(info)) = (&self.observed, &self.info) {
      observed.observer.action_log(info, &message);
    }
    let Some(recorder) = self.recorder.as_ref() else { return };
    recorder.push_event(&TraceEvent::Log(LogEvent {
      call_id: self.call_id.clone(),
      time: recorder.monotonic_ms(),
      message,
    }));
  }

  /// Emit the `input` marker: input-time snapshot name and/or the
  /// viewport point the input was dispatched at.
  pub fn mark_input(&self, input_snapshot: Option<String>, point: Option<(f64, f64)>) {
    let Some(recorder) = self.recorder.as_ref() else { return };
    recorder.bump_screencast_burst();
    recorder.push_event(&TraceEvent::Input(InputActionEvent {
      call_id: self.call_id.clone(),
      input_snapshot,
      point,
    }));
  }

  /// Attach `bytes` to this action (the viewer's Attachments tab); the
  /// body is stored as a sha1-named resource.
  pub fn attach(&mut self, name: impl Into<String>, content_type: impl Into<String>, bytes: Vec<u8>) {
    let Some(recorder) = self.recorder.as_ref() else { return };
    let content_type = content_type.into();
    let ext = attachment_extension(&content_type);
    let sha1 = format!("{}.{ext}", crate::tracing::sha1_hex(&bytes));
    // `attachments: false` keeps the NAME on the action — the viewer
    // still lists what was attached — while leaving the body out of the
    // zip, which is the whole point of the switch.
    if recorder.attachments {
      recorder.push_resource(&TraceResource {
        name: sha1.clone(),
        bytes,
      });
    }
    self.attachments.push(TraceAttachment {
      name: name.into(),
      content_type,
      sha1,
    });
  }

  /// Emit the `after` event, recording `error` when the action failed.
  pub fn finish(self, error: Option<&FerriError>) {
    self.finish_error_info(error.map(ActionErrorInfo::from_ferri));
  }

  /// Emit the `after` event with an already-stringified error (spans
  /// opened by external runners carry plain-text failures).
  pub fn finish_message(self, error: Option<String>) {
    self.finish_error_info(error.map(|message| ActionErrorInfo {
      name: "Error".to_string(),
      message,
    }));
  }

  fn finish_error_info(self, error: Option<ActionErrorInfo>) {
    if let (Some(observed), Some(info)) = (&self.observed, &self.info) {
      observed.observer.action_end(
        info,
        observed.started.elapsed(),
        error.as_ref().map(|e| e.message.as_str()),
      );
    }
    let Some(recorder) = self.recorder.as_ref() else { return };
    recorder.bump_screencast_burst();
    let end_time = recorder.monotonic_ms();
    recorder.push_event(&TraceEvent::After(AfterActionEvent {
      call_id: self.call_id,
      end_time,
      error,
      after_snapshot: self.after_snapshot,
      attachments: self.attachments,
    }));
  }
}

/// How a locator method dispatches input — decides whether the action
/// gets an `input` event, an `input@` snapshot, and a pointer point.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKind {
  /// Dispatches pointer input at a viewport point (click family).
  Pointer,
  /// Dispatches keyboard/value input (fill family) — no point.
  Keyboard,
}

/// Input classification for a locator action method, `None` for pure
/// reads (textContent, boundingBox, …).
pub(crate) fn input_action_kind(method: &str) -> Option<InputKind> {
  match method {
    "click" | "dblclick" | "hover" | "tap" | "check" | "uncheck" | "setChecked" | "dragTo" | "selectText" => {
      Some(InputKind::Pointer)
    },
    "fill" | "press" | "pressSequentially" | "type" | "clear" | "selectOption" | "setInputFiles" => {
      Some(InputKind::Keyboard)
    },
    _ => None,
  }
}

/// Resource-name extension for an attachment's content type.
fn attachment_extension(content_type: &str) -> &'static str {
  let essence = content_type.split(';').next().unwrap_or("").trim();
  match essence {
    "image/png" => "png",
    "image/jpeg" => "jpeg",
    "image/webp" => "webp",
    "text/plain" => "txt",
    "text/html" => "html",
    "application/json" => "json",
    "application/zip" => "zip",
    "video/webm" => "webm",
    _ => "dat",
  }
}

/// Start a traced action span when `composite` has an active recorder.
/// Cheap when tracing is off (one mutex-protected map probe). Writes
/// the `before` event immediately.
#[must_use]
pub(crate) fn begin_action(
  composite: Option<&str>,
  class: &'static str,
  method: &str,
  page_id: Option<String>,
  params: serde_json::Value,
) -> Option<ActionSpan> {
  // An inner call of an action already in flight is that action, not a
  // new one.
  if action_in_progress() {
    return None;
  }
  let recorder = composite.and_then(recorder_for);
  let observer = action_observer(composite);
  let gate = action_gate();
  // Neither recording, observing nor gating: the common case, and the only
  // cost is the map probe plus two relaxed atomic loads.
  if recorder.is_none() && observer.is_none() && gate.is_none() {
    return None;
  }
  // `BrowserContext` reads as `browserContext`, not `browsercontext`:
  // only the first letter drops (Playwright's apiName is the client
  // class's own camelCase name).
  let title = format!("{}.{method}", lower_first(class));
  let call_id = recorder
    .as_ref()
    .map_or_else(next_unrecorded_call_id, |r| r.next_call_id());
  let origin = current_call_origin();
  let info = Arc::new(ActionInfo {
    call_id: call_id.clone(),
    class: class.to_string(),
    method: method.to_string(),
    title: title.clone(),
    params: params.clone(),
    location: origin.location.clone(),
    script: origin.script,
  });
  let watch = observer.map(|observer| {
    Box::new(ObservedAction {
      observer,
      started: Instant::now(),
    })
  });
  if let Some(watch) = &watch {
    watch.observer.action_begin(&info);
  }

  let Some(recorder) = recorder else {
    return Some(ActionSpan {
      recorder: None,
      info: Some(info),
      observed: watch,
      gate,
      call_id,
      before_snapshot: None,
      after_snapshot: None,
      attachments: Vec::new(),
    });
  };

  recorder.bump_screencast_burst();
  let start_time = recorder.monotonic_ms();
  let parent_id = recorder.current_parent();
  // Snapshot names are fixed up front; the capture lands as a later
  // `frame-snapshot` line (same contract as Playwright's async
  // `captureSnapshot` — a failed capture leaves a dangling name the
  // viewer tolerates).
  let before_snapshot = (recorder.snapshots && page_id.is_some()).then(|| format!("before@{call_id}"));
  let stack: Vec<StackFrame> = origin.location.into_iter().collect();
  for frame in &stack {
    recorder.embed_source(&frame.file);
  }
  recorder.push_event(&TraceEvent::Before(BeforeActionEvent {
    call_id: call_id.clone(),
    start_time,
    class: class.to_string(),
    method: method.to_string(),
    title,
    params,
    page_id,
    parent_id,
    // A browser call is its own step unless a runner claims it as part of
    // one; v8 wants the field present either way.
    step_id: Some(call_id.clone()),
    before_snapshot: before_snapshot.clone(),
    stack,
  }));
  Some(ActionSpan {
    recorder: Some(recorder),
    info: Some(info),
    observed: watch,
    gate,
    call_id,
    before_snapshot,
    after_snapshot: None,
    attachments: Vec::new(),
  })
}

/// Hold the action at the gate, if one is installed, before it runs.
///
/// Threaded through the three places a span is opened rather than folded
/// into [`begin_action`] so the pause lands after the before-snapshot is
/// captured: a client that attaches while the action is held should see
/// the same page the trace recorded, not one frame earlier.
pub(crate) async fn open_action(span: Option<ActionSpan>) -> Option<ActionSpan> {
  if let Some(span) = &span
    && let (Some(gate), Some(info)) = (&span.gate, &span.info)
  {
    gate.before_action(info).await;
  }
  span
}

/// A class name as it reads in an API call: `Page` -> `page`,
/// `BrowserContext` -> `browserContext`.
fn lower_first(class: &str) -> String {
  let mut chars = class.chars();
  match chars.next() {
    Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
    None => String::new(),
  }
}

/// Call ids for spans that exist only for an observer: no recorder owns the
/// counter, so they draw from a process-global one.
fn next_unrecorded_call_id() -> String {
  static NEXT: AtomicU64 = AtomicU64::new(1);
  format!("call@{}", NEXT.fetch_add(1, Ordering::Relaxed))
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
  /// Reporter-visible step id this action IS, when the caller has one —
  /// what lets a UI line its test-step tree up with the trace. Defaults
  /// to the action's own call id.
  pub step_id: Option<String>,
  /// Shift the span's start time into the past (spans recorded after
  /// the fact).
  pub backdate_ms: f64,
  /// Call-site stack frames (the viewer's Source tab; a
  /// `sources: true` recording embeds each referenced file).
  pub stack: Vec<StackFrame>,
}

/// Record a failure that belongs to the run rather than to one call —
/// what a test runner writes when a test fails, so the viewer's Errors
/// tab shows the assertion and not only the call that raised it
/// (`testTracing.ts::appendForError`).
///
/// No-op when `composite` is not being traced.
pub fn record_error(composite: &str, message: impl Into<String>, stack: Vec<StackFrame>) {
  let Some(recorder) = recorder_for(composite) else {
    return;
  };
  recorder.push_event(&TraceEvent::Error(TraceErrorEvent {
    message: message.into(),
    stack,
  }));
}

/// Open a titled action span on the active recorder for `composite`.
/// Returns `None` when the composite is not being traced. Writes the
/// `before` event immediately.
#[must_use]
pub fn begin_custom_action(composite: &str, action: CustomAction) -> Option<ActionSpan> {
  let recorder = recorder_for(composite)?;
  recorder.bump_screencast_burst();
  for frame in &action.stack {
    recorder.embed_source(&frame.file);
  }
  let start_time = (recorder.monotonic_ms() - action.backdate_ms).max(0.0);
  let call_id = recorder.next_call_id();
  recorder.push_event(&TraceEvent::Before(BeforeActionEvent {
    call_id: call_id.clone(),
    start_time,
    class: action.class.to_string(),
    method: action.method.to_string(),
    title: action.title,
    params: action.params,
    page_id: None,
    parent_id: action.parent_id,
    step_id: Some(action.step_id.unwrap_or_else(|| call_id.clone())),
    before_snapshot: None,
    stack: action.stack,
  }));
  Some(ActionSpan {
    recorder: Some(recorder),
    info: None,
    observed: None,
    gate: None,
    call_id,
    before_snapshot: None,
    after_snapshot: None,
    attachments: Vec::new(),
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Stands in for a public action builder: `#[track_caller]` makes
  /// `call_origin_here` report this function's CALLER, which is what puts a
  /// user's `.rs` line on the action rather than the builder's body.
  #[track_caller]
  fn builder() -> CallOrigin {
    call_origin_here()
  }

  /// Records which observer saw which action, for the scoping tests.
  #[derive(Debug)]
  struct Recording(&'static str, std::sync::Mutex<Vec<String>>);

  impl ActionObserver for Recording {
    fn action_begin(&self, action: &ActionInfo) {
      if let Ok(mut seen) = self.1.lock() {
        seen.push(format!("{}:{}", self.0, action.title));
      }
    }
    fn action_end(&self, _action: &ActionInfo, _elapsed: std::time::Duration, _error: Option<&str>) {}
    fn action_log(&self, _action: &ActionInfo, _message: &str) {}
  }

  fn recording(tag: &'static str) -> Arc<Recording> {
    Arc::new(Recording(tag, std::sync::Mutex::new(Vec::new())))
  }

  fn seen(r: &Arc<Recording>) -> Vec<String> {
    r.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
  }

  // One test, not three: the observer registry is process-global, so
  // separate #[test] fns would race each other under the default harness.
  #[test]
  fn session_observers_scope_actions_and_unregister_on_drop() {
    let unobserved = begin_action(Some("s:a"), "Page", "goto", None, serde_json::json!({}));
    assert!(unobserved.is_none(), "no observer, no recorder => no span at all");

    let a = recording("a");
    let b = recording("b");
    let guard_a = observe_session_actions("s:a", a.clone());
    let guard_b = observe_session_actions("s:b", b.clone());

    drop(begin_action(Some("s:a"), "Page", "goto", None, serde_json::json!({})));
    drop(begin_action(Some("s:b"), "Page", "click", None, serde_json::json!({})));
    // A session with no observer of its own, and no global: unobserved.
    drop(begin_action(Some("s:c"), "Page", "fill", None, serde_json::json!({})));

    assert_eq!(seen(&a), vec!["a:page.goto".to_string()]);
    assert_eq!(seen(&b), vec!["b:page.click".to_string()]);

    // A global observer catches sessions that have no scoped one, while the
    // scoped ones keep winning for theirs.
    let global = recording("g");
    set_action_observer(global.clone());
    drop(begin_action(Some("s:c"), "Page", "fill", None, serde_json::json!({})));
    drop(begin_action(Some("s:a"), "Page", "reload", None, serde_json::json!({})));
    assert_eq!(seen(&global), vec!["g:page.fill".to_string()]);
    assert_eq!(seen(&a), vec!["a:page.goto".to_string(), "a:page.reload".to_string()]);

    // Dropping a guard unregisters exactly its session.
    drop(guard_a);
    drop(begin_action(Some("s:a"), "Page", "close", None, serde_json::json!({})));
    assert_eq!(
      seen(&global),
      vec!["g:page.fill".to_string(), "g:page.close".to_string()],
      "an unscoped session falls back to the global observer"
    );
    drop(guard_b);

    // While something is watching, a Rust host's call site comes from
    // `#[track_caller]` — and it chains: the location is the caller of
    // `builder`, not the body that calls `call_origin_here`. That chaining
    // is the whole reason every `Action`-returning builder carries the
    // attribute, and it is what lets `pauseAt` name a line in a `.rs` test.
    let (origin, here) = (builder(), line!());
    let frame = origin.location.expect("a global observer is still installed");
    assert!(frame.file.ends_with("trace.rs"), "call site file: {}", frame.file);
    assert_eq!(frame.line, here, "the caller's line, not the builder's body");

    // …and nothing is captured once nothing is watching, so an ordinary run
    // pays no stack walk.
    *ACTION_OBSERVER
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    ACTION_OBSERVER_INSTALLED.store(false, Ordering::Release);
    assert!(builder().location.is_none(), "unwatched runs capture nothing");
  }

  #[test]
  fn context_options_is_first_line_with_version_8() {
    let line = context_options_line("chromium", 1.0, 0.0, Some("t"), &serde_json::json!({}));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["version"].as_u64(), Some(8));
    assert_eq!(parsed["type"].as_str(), Some("context-options"));
    assert_eq!(parsed["origin"].as_str(), Some("library"));
    assert_eq!(parsed["title"].as_str(), Some("t"));
    assert_eq!(parsed["monotonicTime"].as_f64(), Some(0.0));
  }

  #[test]
  fn chunk_context_options_carries_current_monotonic_time() {
    let line = context_options_line("chromium", 1000.0, 250.0, None, &serde_json::json!({}));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["monotonicTime"].as_f64(), Some(250.0));
    assert_eq!(parsed["wallTime"].as_f64(), Some(1250.0));
  }

  #[test]
  fn before_event_serializes_v8_shape_omitting_absent_optionals() {
    let line = serialize_event(&TraceEvent::Before(BeforeActionEvent {
      call_id: "call@1".into(),
      start_time: 1.0,
      class: "Frame".into(),
      method: "click".into(),
      title: "frame.click".into(),
      params: serde_json::json!({ "selector": "#a" }),
      page_id: Some("page@1".into()),
      parent_id: None,
      step_id: None,
      before_snapshot: Some("before@call@1".into()),
      stack: Vec::new(),
    }));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("before"));
    assert_eq!(parsed["callId"].as_str(), Some("call@1"));
    assert_eq!(parsed["beforeSnapshot"].as_str(), Some("before@call@1"));
    assert!(parsed.get("parentId").is_none(), "absent optionals are omitted");
    assert!(parsed.get("stack").is_none(), "empty stack is omitted");
  }

  #[test]
  fn after_event_serializes_error_and_attachments() {
    let line = serialize_event(&TraceEvent::After(AfterActionEvent {
      call_id: "call@1".into(),
      end_time: 2.0,
      error: Some(ActionErrorInfo {
        name: "TimeoutError".into(),
        message: "Timeout 100ms exceeded".into(),
      }),
      after_snapshot: Some("after@call@1".into()),
      attachments: vec![TraceAttachment {
        name: "screenshot".into(),
        content_type: "image/png".into(),
        sha1: "abc.png".into(),
      }],
    }));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("after"));
    assert_eq!(parsed["error"]["name"].as_str(), Some("TimeoutError"));
    assert_eq!(parsed["attachments"][0]["sha1"].as_str(), Some("abc.png"));
  }

  #[test]
  fn input_and_log_events_serialize() {
    let input = serialize_event(&TraceEvent::Input(InputActionEvent {
      call_id: "call@2".into(),
      input_snapshot: Some("input@call@2".into()),
      point: Some((10.5, 20.0)),
    }));
    let parsed: serde_json::Value = serde_json::from_str(&input).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("input"));
    assert_eq!(parsed["point"]["x"].as_f64(), Some(10.5));

    let log = serialize_event(&TraceEvent::Log(LogEvent {
      call_id: "call@2".into(),
      time: 3.0,
      message: "waiting for locator".into(),
    }));
    let parsed: serde_json::Value = serde_json::from_str(&log).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("log"));
    assert_eq!(parsed["message"].as_str(), Some("waiting for locator"));
  }

  #[test]
  fn export_writes_required_zip_entries() {
    let dir = std::env::temp_dir().join(format!("ferri-trace-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.zip");
    let recorder = TraceRecorder::new(
      &TracingStartOptions::default(),
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::temporary(),
    )
    .expect("spool");
    recorder.push_event(&TraceEvent::Before(BeforeActionEvent {
      call_id: recorder.next_call_id(),
      start_time: recorder.monotonic_ms(),
      class: "Page".into(),
      method: "goto".into(),
      title: "page.goto".into(),
      params: serde_json::json!({ "url": "about:blank" }),
      page_id: None,
      parent_id: None,
      step_id: None,
      before_snapshot: None,
      stack: Vec::new(),
    }));
    recorder.push_event(&TraceEvent::After(AfterActionEvent {
      call_id: "call@1".into(),
      end_time: recorder.monotonic_ms(),
      error: None,
      after_snapshot: None,
      attachments: Vec::new(),
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
    assert_eq!(first["platform"].as_str(), Some(trace_platform()));
    assert!(
      first["playwrightVersion"]
        .as_str()
        .is_some_and(|v| v.starts_with("ferridriver/")),
      "the recorder identifies itself: {first}"
    );
    std::fs::remove_dir_all(&dir).ok();
  }

  /// A recording under a caller's `tracesDir` is named, left in place,
  /// and readable as it is written — the three things a viewer following
  /// a running test depends on.
  #[test]
  fn a_named_live_recording_is_readable_while_it_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let location = TraceLocation::in_dir(dir.path().to_path_buf(), "abc123-test".to_string());
    let recorder = TraceRecorder::new(
      &TracingStartOptions {
        streaming: TraceStreaming::Live,
        ..TracingStartOptions::default()
      },
      "chromium".into(),
      serde_json::json!({}),
      0,
      location,
    )
    .expect("spool");

    recorder.push_event(&TraceEvent::Before(BeforeActionEvent {
      call_id: "call@1".into(),
      start_time: 0.0,
      class: "Page".into(),
      method: "goto".into(),
      title: "page.goto".into(),
      params: serde_json::json!({ "url": "about:blank" }),
      page_id: Some("page@1".into()),
      parent_id: None,
      step_id: Some("call@1".into()),
      before_snapshot: None,
      stack: Vec::new(),
    }));

    // Still recording: no stop, no zip, and the file already has both
    // lines in it.
    let path = dir.path().join("abc123-test.trace");
    let written = std::fs::read_to_string(&path).expect("live trace file");
    assert_eq!(written.lines().count(), 2, "unflushed live trace: {written:?}");
    let action: serde_json::Value = serde_json::from_str(written.lines().nth(1).unwrap()).unwrap();
    assert_eq!(action["stepId"].as_str(), Some("call@1"), "v8 actions carry a stepId");

    drop(recorder);
    assert!(path.exists(), "a caller's tracesDir must survive the recorder");
  }

  #[test]
  fn a_temporary_recording_cleans_up_after_itself() {
    let recorder = TraceRecorder::new(
      &TracingStartOptions::default(),
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::temporary(),
    )
    .expect("spool");
    let dir = recorder.location().dir;
    assert!(dir.exists());
    drop(recorder);
    assert!(!dir.exists(), "temp spool left behind");
  }

  #[test]
  fn a_chunk_can_be_renamed_and_retitled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let recorder = TraceRecorder::new(
      &TracingStartOptions {
        title: Some("first".into()),
        streaming: TraceStreaming::Live,
        ..TracingStartOptions::default()
      },
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::in_dir(dir.path().to_path_buf(), "one".to_string()),
    )
    .expect("spool");

    recorder.start_chunk(0, Some("two".to_string()), Some("second".to_string()));
    assert_eq!(recorder.location().name, "two");
    assert_eq!(recorder.title().as_deref(), Some("second"));

    let second = std::fs::read_to_string(dir.path().join("two.trace")).expect("second chunk");
    let context: serde_json::Value = serde_json::from_str(second.lines().next().unwrap()).unwrap();
    assert_eq!(context["title"].as_str(), Some("second"));
    assert!(dir.path().join("one.trace").exists(), "previous chunk was discarded");
  }

  #[test]
  fn groups_nest_the_actions_recorded_inside_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let recorder = TraceRecorder::new(
      &TracingStartOptions {
        streaming: TraceStreaming::Live,
        ..TracingStartOptions::default()
      },
      "chromium".into(),
      serde_json::json!({}),
      0,
      TraceLocation::in_dir(dir.path().to_path_buf(), "grouped".to_string()),
    )
    .expect("spool");

    recorder.begin_group(
      "checkout".to_string(),
      vec![StackFrame {
        file: "/spec.ts".into(),
        line: 3,
        column: 1,
      }],
    );
    let inner_parent = recorder.current_parent();
    recorder.end_group();
    let after_end = recorder.current_parent();

    let written = std::fs::read_to_string(dir.path().join("grouped.trace")).expect("trace");
    let events: Vec<serde_json::Value> = written.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let group = &events[1];
    assert_eq!(group["type"].as_str(), Some("before"));
    assert_eq!(group["class"].as_str(), Some("Tracing"));
    assert_eq!(group["method"].as_str(), Some("tracingGroup"));
    assert_eq!(group["title"].as_str(), Some("checkout"));
    assert_eq!(group["stack"][0]["line"].as_u64(), Some(3));

    assert_eq!(
      inner_parent.as_deref(),
      group["callId"].as_str(),
      "actions inside a group nest under it"
    );
    assert!(after_end.is_none(), "groupEnd pops the group");
    assert_eq!(events[2]["type"].as_str(), Some("after"));
    assert_eq!(events[2]["callId"], group["callId"]);
  }

  #[test]
  fn run_level_errors_serialize_for_the_errors_tab() {
    let line = serialize_event(&TraceEvent::Error(TraceErrorEvent {
      message: "expect(received).toBe(expected)".into(),
      stack: vec![StackFrame {
        file: "/spec.ts".into(),
        line: 9,
        column: 2,
      }],
    }));
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("valid json");
    assert_eq!(parsed["type"].as_str(), Some("error"));
    assert_eq!(parsed["message"].as_str(), Some("expect(received).toBe(expected)"));
    assert_eq!(parsed["stack"][0]["file"].as_str(), Some("/spec.ts"));
  }
}
