//! Disk-spooled [`TraceRecorder`] and Playwright v8 trace zip exporter.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::error::{FerriError, Result};

use super::SCREENCAST_BURST_MS;
use super::events::{
  AfterActionEvent, BeforeActionEvent, StackFrame, TraceEvent, TraceLocation, TraceResource, TraceStreaming,
  TracingStartOptions, context_options_line, now_epoch_ms, serialize_event,
};

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
#[expect(
  clippy::struct_excessive_bools,
  reason = "carries TracingStartOptions' four independent capture switches for the life of the recording"
)]
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
  /// Each filmed page's screencast subscription, ended by `stop`.
  screencasts: std::sync::Mutex<Vec<crate::backend::screencast::ScreencastSubscription>>,
  /// Browser name recorded in `context-options`.
  browser_name: String,
  /// Context-creation options recorded in `context-options.options`
  /// (viewport etc. — the viewer's Metadata tab).
  context_options: serde_json::Value,
  /// Bumped on every appended event/resource ([`Self::spool_version`]).
  spool_version: AtomicU64,
  /// Snapshot-history epoch for this chunk: page documents compare it
  /// against their stored value at capture time and self-reset on
  /// mismatch (`crate::snapshotter`). Process-unique so a document
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
      screencasts: std::sync::Mutex::new(Vec::new()),
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
  pub(super) fn current_parent(&self) -> Option<String> {
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
  pub(super) fn bump_screencast_burst(&self) {
    let until = self.elapsed_whole_ms().saturating_add(SCREENCAST_BURST_MS);
    self.screencast_burst_until_ms.store(until, Ordering::Relaxed);
  }

  /// Whether the around-action burst window is open now.
  pub(super) fn screencast_burst_active(&self) -> bool {
    self.elapsed_whole_ms() < self.screencast_burst_until_ms.load(Ordering::Relaxed)
  }

  fn elapsed_whole_ms(&self) -> u64 {
    u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
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

  /// Track a page's screencast subscription so `stop` can end it.
  pub(crate) fn track_screencast(&self, subscription: crate::backend::screencast::ScreencastSubscription) {
    self
      .screencasts
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(subscription);
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
  pub async fn stop_screencasts(&self) {
    let subscriptions: Vec<_> = std::mem::take(
      &mut *self
        .screencasts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for subscription in subscriptions {
      subscription.stop().await;
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
    .is_some_and(|ext| matches!(ext, "jpeg" | "jpg" | "png" | "webp" | "zip" | "webm" | "mp4"))
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
/// Stop kills the cycle's process group) never runs `TraceSpool`'s
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
    if pid == own_pid || crate::backend::process::process_is_live(pid) {
      continue;
    }
    let _ = std::fs::remove_dir_all(entry.path());
  }
}
