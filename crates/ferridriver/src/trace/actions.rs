//! Traced action spans, call-origin tracking, pending-call registry, and action observers/gates.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::error::FerriError;

use super::events::{
  ActionErrorInfo, AfterActionEvent, BeforeActionEvent, InputActionEvent, LogEvent, StackFrame, TraceAttachment,
  TraceErrorEvent, TraceEvent, TraceResource,
};
use super::recorder::TraceRecorder;
use super::{RECORDING_ACTIVE, recorder_for};

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
  /// Set while an assertion looks at the page.
  static IN_ASSERTION: ();
}

/// Run `fut` as the body of an assertion. The calls it makes to look at
/// the page are recorded in the trace as before, but no observer is told
/// about them: whoever watches the action stream sees the assertion as
/// one action, as Playwright's `Frame.expect` is one call.
pub fn within_assertion<F: std::future::Future>(fut: F) -> impl std::future::Future<Output = F::Output> {
  IN_ASSERTION.scope((), fut)
}

fn assertion_in_progress() -> bool {
  IN_ASSERTION.try_with(|()| ()).is_ok()
}

tokio::task_local! {
  /// Origin of the API call the current future is performing.
  ///
  /// The host scopes this at the language boundary, where the caller's
  /// stack is still live; core reads it several awaits later, inside
  /// `begin_action`. A task-local and not a slot because
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
    || PENDING_TRACKING.load(Ordering::Acquire)
}

fn current_call_origin() -> CallOrigin {
  let mut origin = CALL_ORIGIN.try_with(Clone::clone).unwrap_or_default();
  if origin.script.is_none() {
    origin.script = CALL_OWNER.try_with(Arc::clone).ok();
  }
  origin
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

// ── Pending calls ──────────────────────────────────────────────────────
//
// A test that runs out of time should say which call it was stuck in, as
// Playwright reports the interrupted action with its call log. A test may
// drive a browser it launched itself, so calls are filed by who issued them
// (the script identity of a [`CallOrigin`], or [`with_call_owner`] for a
// host with no script), not by the context they ran in.

pub(super) static PENDING_TRACKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static PENDING_CALLS: std::sync::Mutex<Option<rustc_hash::FxHashMap<String, PendingCallEntry>>> =
  std::sync::Mutex::new(None);

tokio::task_local! {
  /// Who issued the calls of a host that has no script identity: a Rust
  /// test body, filed under its test.
  pub(super) static CALL_OWNER: Arc<str>;
}

struct PendingCallEntry {
  owner: Arc<str>,
  call: PendingCall,
  /// When the call was dropped unfinished. A timed-out test is torn down
  /// by dropping its body, so its calls end this way; the entry outlives
  /// the drop until the owner reads or forgets it.
  dropped_at: Option<Instant>,
}

/// A call that had not finished.
#[derive(Clone)]
pub struct PendingCall {
  /// `page.click`, `locator.fill`, ...
  pub title: String,
  /// Where it was written, when the host captured it.
  pub location: Option<StackFrame>,
  /// Its call log so far, oldest first.
  pub log: Vec<String>,
}

/// Start filing calls by their owner. Off by default: only a host that can
/// report on a stuck owner pays for the bookkeeping.
pub fn track_pending_calls() {
  PENDING_TRACKING.store(true, Ordering::Release);
}

/// Run `fut` with `owner` as the issuer of the calls it makes, for a host
/// whose calls carry no script identity.
pub fn with_call_owner<F: std::future::Future>(
  owner: Arc<str>,
  fut: F,
) -> impl std::future::Future<Output = F::Output> {
  CALL_OWNER.scope(owner, fut)
}

/// The calls `owner` had in flight at `deadline`: those still running, and
/// those dropped at or after it.
#[must_use]
pub fn pending_calls(owner: &str, deadline: Instant) -> Vec<PendingCall> {
  let guard = PENDING_CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  let Some(map) = guard.as_ref() else { return Vec::new() };
  let mut calls: Vec<(&String, &PendingCallEntry)> = map
    .iter()
    .filter(|(_, entry)| &*entry.owner == owner && entry.dropped_at.is_none_or(|at| at >= deadline))
    .collect();
  calls.sort_by_key(|(id, _)| call_number(id));
  calls.into_iter().map(|(_, entry)| entry.call.clone()).collect()
}

/// Drop every record `owner` left behind.
pub fn forget_pending_calls(owner: &str) {
  let mut guard = PENDING_CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  if let Some(map) = guard.as_mut() {
    map.retain(|_, entry| &*entry.owner != owner);
  }
}

fn call_number(call_id: &str) -> u64 {
  call_id.rsplit('@').next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// A span's hold on its registry entry: removed when the call finishes,
/// stamped as dropped when the span goes without finishing.
struct PendingGuard {
  call_id: String,
}

impl PendingGuard {
  fn open(call_id: &str, title: &str, origin: &CallOrigin) -> Option<Self> {
    let owner = Arc::clone(origin.script.as_ref()?);
    let mut guard = PENDING_CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.get_or_insert_with(rustc_hash::FxHashMap::default).insert(
      call_id.to_string(),
      PendingCallEntry {
        owner,
        call: PendingCall {
          title: title.to_string(),
          location: origin.location.clone(),
          log: Vec::new(),
        },
        dropped_at: None,
      },
    );
    Some(Self {
      call_id: call_id.to_string(),
    })
  }

  fn log(&self, message: &str) {
    let mut guard = PENDING_CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = guard.as_mut().and_then(|map| map.get_mut(&self.call_id)) {
      entry.call.log.push(message.to_string());
    }
  }

  /// Removes the entry, so the `Drop` that follows finds nothing to stamp.
  fn finished(self) {
    let mut guard = PENDING_CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(map) = guard.as_mut() {
      map.remove(&self.call_id);
    }
    drop(guard);
  }
}

impl Drop for PendingGuard {
  fn drop(&mut self) {
    let mut guard = PENDING_CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = guard.as_mut().and_then(|map| map.get_mut(&self.call_id)) {
      entry.dropped_at = Some(Instant::now());
    }
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
pub(super) static ACTION_OBSERVER: std::sync::RwLock<Option<Arc<dyn ActionObserver>>> = std::sync::RwLock::new(None);

/// Per-session observers, keyed by composite session key.
///
/// A process that hosts several sessions at once (a bound browser serving
/// attached clients) needs each client to see ITS actions and no one else's.
/// Actions already carry the composite they belong to, so scoping is a lookup
/// rather than any new plumbing through the call sites.
static SESSION_ACTION_OBSERVERS: ObserverMap = std::sync::RwLock::new(None);

/// Per-owner observers, keyed by the identity calls are filed under
/// ([`CallOrigin::script`]).
///
/// A test runner reports each test's browser calls as steps of that test,
/// whichever context or browser the test drove — a test may open contexts
/// of its own. Its calls already carry the identity of the test that made
/// them, so that is the key, not the session.
static OWNER_ACTION_OBSERVERS: ObserverMap = std::sync::RwLock::new(None);

/// True when a global, session or owner observer exists. Keeps the
/// unobserved hot path at one relaxed load rather than lock acquisitions.
pub(super) static ACTION_OBSERVER_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
    unregister_observer(&SESSION_ACTION_OBSERVERS, &self.composite);
  }
}

/// Observe only the actions issued by `owner` (the identity a host files
/// its calls under, [`CallOrigin::script`]), until the returned guard
/// drops. Wins over a session or global observer for those actions.
#[must_use]
pub fn observe_owner_actions(owner: &str, observer: Arc<dyn ActionObserver>) -> OwnerObserverGuard {
  {
    let mut guard = OWNER_ACTION_OBSERVERS
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
      .get_or_insert_with(rustc_hash::FxHashMap::default)
      .insert(owner.to_string(), observer);
  }
  ACTION_OBSERVER_INSTALLED.store(true, Ordering::Release);
  OwnerObserverGuard {
    owner: owner.to_string(),
  }
}

/// Removes its owner's action observer on drop.
pub struct OwnerObserverGuard {
  owner: String,
}

impl Drop for OwnerObserverGuard {
  fn drop(&mut self) {
    unregister_observer(&OWNER_ACTION_OBSERVERS, &self.owner);
  }
}

type ObserverMap = std::sync::RwLock<Option<rustc_hash::FxHashMap<String, Arc<dyn ActionObserver>>>>;

fn unregister_observer(registry: &ObserverMap, key: &str) {
  let mut guard = registry.write().unwrap_or_else(std::sync::PoisonError::into_inner);
  let Some(map) = guard.as_mut() else { return };
  map.remove(key);
  if !map.is_empty() {
    return;
  }
  *guard = None;
  drop(guard);
  // Only clear the fast-path flag when no other observer remains, or a
  // `run --trace` in the same process would go silent.
  let global_present = ACTION_OBSERVER
    .read()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .is_some();
  let others_present = [&SESSION_ACTION_OBSERVERS, &OWNER_ACTION_OBSERVERS]
    .into_iter()
    .any(|other| {
      other
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_some()
    });
  if !global_present && !others_present {
    ACTION_OBSERVER_INSTALLED.store(false, Ordering::Release);
  }
}

fn action_observer(composite: Option<&str>, owner: Option<&str>) -> Option<Arc<dyn ActionObserver>> {
  if !ACTION_OBSERVER_INSTALLED.load(Ordering::Acquire) || assertion_in_progress() {
    return None;
  }
  if let Some(owner) = owner {
    let scoped = OWNER_ACTION_OBSERVERS
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(observer) = scoped.as_ref().and_then(|map| map.get(owner)) {
      return Some(Arc::clone(observer));
    }
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

/// An in-flight traced action. `begin_action` /
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
  /// This call's entry in the pending-call registry, while it runs.
  pending: Option<PendingGuard>,
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
    if let Some(pending) = &self.pending {
      pending.log(&message);
    }
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

  fn finish_error_info(mut self, error: Option<ActionErrorInfo>) {
    if let Some(pending) = self.pending.take() {
      pending.finished();
    }
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
    "video/mp4" => "mp4",
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
  let gate = action_gate();
  if recorder.is_none() && gate.is_none() && !observed_or_tracked() {
    return None;
  }
  let origin = current_call_origin();
  let observer = action_observer(composite, origin.script.as_deref());
  let watched = recorder.is_some() || observer.is_some() || gate.is_some();
  // Tracking alone opens a span only for a call someone owns.
  let tracked = PENDING_TRACKING.load(Ordering::Acquire) && origin.script.is_some();
  if !watched && !tracked {
    return None;
  }
  // `BrowserContext` reads as `browserContext`, not `browsercontext`:
  // only the first letter drops (Playwright's apiName is the client
  // class's own camelCase name).
  let title = format!("{}.{method}", lower_first(class));
  let call_id = recorder
    .as_ref()
    .map_or_else(next_unrecorded_call_id, |r| r.next_call_id());
  let pending = if tracked {
    PendingGuard::open(&call_id, &title, &origin)
  } else {
    None
  };
  if !watched {
    return Some(ActionSpan {
      recorder: None,
      info: None,
      observed: None,
      gate: None,
      call_id,
      before_snapshot: None,
      after_snapshot: None,
      attachments: Vec::new(),
      pending,
    });
  }
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
      pending,
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
    pending,
  })
}

/// Whether an observer or call tracking could want an action. With no
/// recorder and no gate either — the common case — an action costs the
/// map probe plus these relaxed atomic loads.
fn observed_or_tracked() -> bool {
  ACTION_OBSERVER_INSTALLED.load(Ordering::Acquire) || PENDING_TRACKING.load(Ordering::Acquire)
}

/// Hold the action at the gate, if one is installed, before it runs.
///
/// Threaded through the three places a span is opened rather than folded
/// into `begin_action` so the pause lands after the before-snapshot is
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
    pending: None,
  })
}
