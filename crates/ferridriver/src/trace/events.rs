//! Playwright trace v8 event schemas, option bags, spool locations, and JSONL serializers.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::FerriError;

/// Trace format version this recorder emits.
pub(super) const TRACE_VERSION: u32 = 8;

/// Options bag for `tracing.start` (Playwright:
/// `tracing.start({ name?, title?, screenshots?, snapshots?, sources? })`).
#[expect(
  clippy::struct_excessive_bools,
  reason = "Playwright's tracing.start option bag: screenshots, snapshots, sources and attachments are independent switches, not states of one thing"
)]
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
  pub(super) fn from_ferri(error: &FerriError) -> Self {
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

  pub(super) fn trace_file(&self) -> std::path::PathBuf {
    self.dir.join(format!("{}.trace", self.name))
  }

  pub(super) fn network_file(&self) -> std::path::PathBuf {
    self.dir.join(format!("{}.network", self.name))
  }
}

/// Platform string in the form the viewer's metadata pane (and every
/// Playwright trace ever recorded) uses: node's `process.platform`, not
/// Rust's `std::env::consts::OS`.
pub(super) fn trace_platform() -> &'static str {
  match std::env::consts::OS {
    "macos" => "darwin",
    "windows" => "win32",
    other => other,
  }
}

/// What produced the trace, shown in the viewer's metadata pane where
/// Playwright puts its own version.
pub(super) fn recorder_version() -> String {
  format!("ferridriver/{}", env!("CARGO_PKG_VERSION"))
}

/// First trace line: `context-options` with `version: 8` (the loader
/// mis-modernizes everything as v6 without it). `monotonic` anchors
/// the chunk's start on the timeline (0 for a fresh recording, the
/// current clock for later chunks).
pub(super) fn context_options_line(
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

pub(super) fn serialize_event(event: &TraceEvent) -> String {
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

pub(super) fn now_epoch_ms() -> f64 {
  let now = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or_default();
  now.as_secs_f64() * 1000.0
}
