//! Transport trait and shared CDP message dispatch logic.
//!
//! The dispatch logic (response correlation, document-accurate lifecycle
//! tracking, event broadcast) is identical for pipe and WebSocket
//! transports. It lives here as `CdpDispatcher` — both transports embed
//! it and call `dispatch_message`.
//!
//! Navigation waits are driven entirely off the per-session
//! [`super::LifecycleState`] (commit + lifecycle, both gated on the
//! navigation's `loaderId`). `Page.loadEventFired` / `Page.domContentEventFired`
//! are deliberately ignored — they're page-level events with no
//! loaderId, so a late-arriving `loadEventFired` from a previous
//! document can resolve a fresh wait before the new document has even
//! committed (see `Frame.gotoImpl` in
//! `/tmp/playwright/packages/playwright-core/src/server/frames.ts` —
//! Playwright likewise drives navigation off `Page.lifecycleEvent` only).

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{broadcast, oneshot};

use crate::backend::json_scan;
use crate::error::{FerriError, Result};

/// Truncate a string for logging, appending "..." if truncated.
fn truncate_for_log(s: &str, max: usize) -> String {
  if s.len() <= max {
    s.to_string()
  } else {
    format!("{}...", &s[..max])
  }
}

/// Result of a single CDP command: either the response value or a typed error.
type CdpResult = Result<serde_json::Value>;

/// In-flight CDP command entry. Carries the response oneshot plus the
/// method name and send timestamp so [`RttStats`] can attribute the
/// observed round-trip latency to the right CDP method when the
/// response lands. The method name is stored as `String` because CDP
/// method strings come from arbitrary callers (`&str`) and we need
/// owned storage to outlive the borrow; the alloc is amortised
/// against the per-command serialization cost (~µs scale, much
/// larger than the alloc itself).
pub(crate) struct PendingEntry {
  tx: oneshot::Sender<CdpResult>,
  session_id: Option<Box<str>>,
  method: String,
  send_at: Option<Instant>,
}

/// Pending-command map: command ID -> [`PendingEntry`].
///
/// Sharded via [`dashmap::DashMap`] so concurrent senders don't
/// serialise on a single global mutex. Each shard has its own
/// internal `RwLock`; insert/remove on different keys are wait-free
/// vs each other. Replaced `Arc<std::sync::Mutex<FxHashMap>>` —
/// uncontended insert went from ~100ns (mutex acq + `HashMap` insert)
/// to ~50ns (shard lookup + per-shard insert), and contention at
/// 4+ concurrent senders no longer serialises. The per-key mutex
/// model would be even cheaper but requires `parking_lot::Mutex`
/// per entry which complicates the lifetime story for one-shot
/// senders. `DashMap` is the right balance.
pub(crate) type PendingMap = DashMap<u64, PendingEntry>;

/// Aggregated per-CDP-method round-trip statistics. Updated when a
/// response lands in [`CdpDispatcher::dispatch_message`] and the
/// matching `PendingEntry` is removed. Dumped to stderr on
/// [`CdpDispatcher::drop`] when `FERRIDRIVER_RTT_STATS=1` is set.
#[derive(Default)]
struct RttBucket {
  count: u64,
  total_ns: u128,
  max_ns: u128,
}

/// Format nanoseconds as fixed-point milliseconds with 2 decimals,
/// using integer arithmetic to dodge the `u128 -> f64` precision-loss
/// lint without suppressions.
fn fmt_ms2(ns: u128) -> String {
  let ms = ns / 1_000_000;
  let dec = (ns % 1_000_000) / 10_000;
  format!("{ms}.{dec:02}")
}

/// Format nanoseconds as fixed-point microseconds with 1 decimal.
fn fmt_us1(ns: u128) -> String {
  let us = ns / 1_000;
  let dec = (ns % 1_000) / 100;
  format!("{us}.{dec}")
}

/// Average microseconds with 1 decimal — `total_ns / count / 1000`
/// in integer space to dodge precision-loss lint.
fn fmt_avg_us1(total_ns: u128, count: u64) -> String {
  if count == 0 {
    return "0.0".to_string();
  }
  let total_us10 = total_ns * 10 / 1_000;
  let avg_us10 = total_us10 / u128::from(count);
  let us = avg_us10 / 10;
  let dec = avg_us10 % 10;
  format!("{us}.{dec}")
}

#[derive(Default)]
pub(crate) struct RttStats {
  buckets: FxHashMap<String, RttBucket>,
}

impl RttStats {
  fn record(&mut self, method: &str, elapsed_ns: u128) {
    let entry = self.buckets.entry(method.to_string()).or_default();
    entry.count += 1;
    entry.total_ns += elapsed_ns;
    if elapsed_ns > entry.max_ns {
      entry.max_ns = elapsed_ns;
    }
  }

  fn merge(&mut self, other: &RttStats) {
    for (method, b) in &other.buckets {
      let entry = self.buckets.entry(method.clone()).or_default();
      entry.count += b.count;
      entry.total_ns += b.total_ns;
      if b.max_ns > entry.max_ns {
        entry.max_ns = b.max_ns;
      }
    }
  }

  fn dump(&self) {
    if self.buckets.is_empty() {
      return;
    }
    let mut rows: Vec<(&String, &RttBucket)> = self.buckets.iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.total_ns));
    let total_count: u64 = self.buckets.values().map(|b| b.count).sum();
    let total_ns: u128 = self.buckets.values().map(|b| b.total_ns).sum();
    eprintln!(
      "─── ferridriver CDP RTT stats ─── total_calls={total_count}  total_time={}ms",
      fmt_ms2(total_ns)
    );
    eprintln!(
      "  {:<48}  {:>7}  {:>10}  {:>10}  {:>10}",
      "method", "count", "total_ms", "avg_us", "max_us"
    );
    for (method, bucket) in rows {
      eprintln!(
        "  {:<48}  {:>7}  {:>10}  {:>10}  {:>10}",
        method,
        bucket.count,
        fmt_ms2(bucket.total_ns),
        fmt_avg_us1(bucket.total_ns, bucket.count),
        fmt_us1(bucket.max_ns),
      );
    }
  }
}

/// Returns true when the `FERRIDRIVER_RTT_STATS` env var is set to
/// any truthy value (`1`, `true`, `yes`). Cached via [`std::sync::OnceLock`]
/// so the env-var lookup happens once per process. First truthy
/// observation also registers a libc-level `atexit` hook that dumps
/// the aggregated [`global_rtt_stats`] — covers process-exit paths
/// (NAPI / cargo-test harness) where individual dispatcher Drops do
/// not run because reader/writer tokio tasks still hold Arc clones.
fn rtt_stats_enabled() -> bool {
  static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  *ENABLED.get_or_init(|| {
    let on = std::env::var("FERRIDRIVER_RTT_STATS").is_ok_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"));
    if on {
      // SAFETY: atexit takes a `extern "C" fn()` and stores it in a
      // process-global list. The handler reads from a `Mutex<RttStats>`
      // owned by a `OnceLock` so its lifetime spans process exit.
      #[allow(unsafe_code)]
      unsafe {
        libc::atexit(rtt_atexit_dump);
      }
    }
    on
  })
}

/// Process-global aggregate of every [`CdpDispatcher`]'s RTT buckets.
/// Each dispatcher merges its local stats here on drop; the libc
/// atexit hook registered by [`rtt_stats_enabled`] dumps the global
/// table on process exit (covers `process::exit` / NAPI paths where
/// per-task dispatcher drops never run).
fn global_rtt_stats() -> &'static std::sync::Mutex<RttStats> {
  static GLOBAL: std::sync::OnceLock<std::sync::Mutex<RttStats>> = std::sync::OnceLock::new();
  GLOBAL.get_or_init(|| std::sync::Mutex::new(RttStats::default()))
}

extern "C" fn rtt_atexit_dump() {
  if let Ok(stats) = global_rtt_stats().lock()
    && !stats.buckets.is_empty()
  {
    stats.dump();
  }
}

/// Explicit dump entry-point for runtimes whose process-exit path
/// doesn't trigger libc `atexit` (Bun + some Node configurations).
/// CLI bridges call this just before `process.exit` so the
/// `FERRIDRIVER_RTT_STATS=1` table prints reliably.
pub fn dump_global_rtt_stats() {
  if !rtt_stats_enabled() {
    return;
  }
  if let Ok(stats) = global_rtt_stats().lock()
    && !stats.buckets.is_empty()
  {
    stats.dump();
  }
}

/// Trait abstracting CDP transport medium (pipes vs WebSocket).
pub trait CdpTransport: Send + Sync + 'static {
  fn close(&self) -> impl std::future::Future<Output = Result<()>> + Send;

  /// Whether the browser connection has ended (EOF on the pipe/socket).
  fn is_disconnected(&self) -> bool;

  fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send;

  fn subscribe_events(&self) -> broadcast::Receiver<Arc<serde_json::Value>>;

  fn subscribe_event_method(&self, method: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>>;

  fn subscribe_event_domain(&self, domain: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>>;

  /// Lossless, wire-ordered event tap for the given exact CDP methods,
  /// optionally filtered to one session. Unlike the broadcast
  /// subscriptions above, a tap never drops events — state-mutating
  /// consumers (frame/context maps, network correlation, dialog /
  /// download / file-chooser registries, Fetch interception) MUST use
  /// taps; a dropped event there corrupts the map or hangs the page.
  fn tap_event_methods(
    &self,
    methods: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>>;

  /// Lossless, wire-ordered event tap for whole CDP domains
  /// (`Network`, `Runtime`, ...). Registering several domains on one
  /// tap yields a single stream ordered across those domains exactly
  /// as the events arrived on the wire.
  fn tap_event_domains(
    &self,
    domains: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>>;

  /// Lossless, wire-ordered tap of EVERY event carried by exactly one
  /// session. Strict session match: browser-scoped events do not leak
  /// into a page-session stream. Backs the public `CDPSession` events.
  fn tap_all_events(&self, session_id: &str) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>>;

  fn tap_iframe_targets(&self) -> IframeTargetSubscription;

  fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
    frame_observer: super::transport::FrameStateObserver,
  );

  /// Release everything the dispatcher holds for a page session that is
  /// going away. Taps are otherwise only pruned when an event of that
  /// exact method arrives, and the lifecycle tracker was never removed
  /// at all — both grew by an entry per page for the life of the
  /// browser.
  fn unregister_session(&self, session_id: &str);
}

// ── Shared dispatch state ──────────────────────────────────────────────────

pub type FrameStateObserver = Arc<dyn Fn(&[u8], &str) + Send + Sync>;

pub(crate) fn frame_state_observer(
  cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  events: crate::events::EventEmitter,
) -> FrameStateObserver {
  make_frame_state_observer(cache, events, None)
}

pub(crate) fn frame_state_observer_for_renderer(
  cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  events: crate::events::EventEmitter,
  frame: String,
  parent: String,
) -> FrameStateObserver {
  make_frame_state_observer(cache, events, Some((frame, parent)))
}

fn make_frame_state_observer(
  cache: Arc<std::sync::Mutex<crate::frame_cache::FrameCache>>,
  events: crate::events::EventEmitter,
  renderer: Option<(String, String)>,
) -> FrameStateObserver {
  Arc::new(move |raw, method| {
    if method == "Page.frameDetached" {
      let params = json_scan::json_field(raw, b"params");
      if json_scan::json_string(json_scan::json_field(params, b"reason")) == b"swap" {
        if let Ok(frame) = std::str::from_utf8(json_scan::json_string(json_scan::json_field(params, b"frameId"))) {
          lock_or_recover(&cache).detach_descendants(frame);
        }
        return;
      }
    }
    if let Some(mut event) = frame_event(raw, method) {
      if let Some((frame, parent)) = &renderer
        && let crate::events::PageEvent::FrameNavigated(info) = &mut event
        && info.frame_id == *frame
      {
        info.parent_frame_id = Some(parent.clone());
      }
      {
        let mut cache = lock_or_recover(&cache);
        match &event {
          crate::events::PageEvent::FrameAttached(info) => cache.attach(info.clone()),
          crate::events::PageEvent::FrameDetached { frame_id } => cache.detach(frame_id),
          crate::events::PageEvent::FrameNavigated(info) => cache.navigated(info.clone()),
          crate::events::PageEvent::FrameNavigatedWithinDocument(info) => {
            cache.navigated_within(&info.frame_id, &info.url);
          },
          _ => {},
        }
      }
      events.emit(event);
    }
  })
}

pub(crate) struct LifecycleTracker {
  pub state: Arc<std::sync::Mutex<super::LifecycleState>>,
  pub notify: Arc<tokio::sync::Notify>,
  pub frame_observer: super::transport::FrameStateObserver,
}

pub(crate) struct PendingCommand<'a> {
  dispatcher: &'a CdpDispatcher,
  id: Option<u64>,
}

impl PendingCommand<'_> {
  pub(crate) fn completed(&mut self) {
    self.id = None;
  }
}

impl Drop for PendingCommand<'_> {
  fn drop(&mut self) {
    if let Some(id) = self.id {
      self.dispatcher.forget_pending(id);
    }
  }
}

#[derive(Default)]
struct ContextSnapshots(FxHashMap<String, FxHashMap<String, CachedContext>>);

struct CachedContext {
  id: i64,
  event: Vec<u8>,
}

impl ContextSnapshots {
  fn observe(&mut self, session: &str, method: &str, raw: &[u8]) {
    if method == "Runtime.executionContextsCleared" {
      self.0.remove(session);
      return;
    }
    let Ok(event) = serde_json::from_slice::<serde_json::Value>(raw) else {
      return;
    };
    let params = &event["params"];
    match method {
      "Runtime.executionContextCreated" => {
        let context = &params["context"];
        if context["auxData"]["isDefault"] == true
          && let (Some(frame), Some(id)) = (context["auxData"]["frameId"].as_str(), context["id"].as_i64())
        {
          self.0.entry(session.to_owned()).or_default().insert(
            frame.to_owned(),
            CachedContext {
              id,
              event: raw.to_vec(),
            },
          );
        }
      },
      "Runtime.executionContextDestroyed" => {
        if let Some(id) = params["executionContextId"].as_i64()
          && let Some(contexts) = self.0.get_mut(session)
        {
          contexts.retain(|_, context| context.id != id);
        }
      },
      "Page.frameDetached" => {
        if let Some(frame) = params["frameId"].as_str()
          && let Some(contexts) = self.0.get_mut(session)
        {
          contexts.remove(frame);
        }
      },
      _ => {},
    }
    if self.0.get(session).is_some_and(FxHashMap::is_empty) {
      self.0.remove(session);
    }
  }
}

/// Shared CDP message dispatch state. Embedded by both `PipeTransport` and `WsTransport`.
pub(crate) struct CdpDispatcher {
  /// Set once by the reader task on EOF — see [`Self::is_disconnected`].
  disconnected: std::sync::atomic::AtomicBool,
  pub next_id: AtomicU64,
  pub pending: Arc<PendingMap>,
  /// Per-session lifecycle trackers (keyed by sessionId). Sharded
  /// via `DashMap` so events firing on N sessions don't contend on
  /// the same mutex.
  lifecycle_trackers: Arc<DashMap<String, LifecycleTracker>>,
  iframe_targets: std::sync::Mutex<IframeTargets>,
  context_snapshots: std::sync::Mutex<ContextSnapshots>,
  /// Per-message broadcast channel. Wraps the message in `Arc` so
  /// fanout to N subscribers is N refcount bumps (~5ns each)
  /// instead of N deep `serde_json::Value` clones (~400ns + ~10
  /// allocs each). At 200 events/s × ~12 subscribers per page this
  /// is the single biggest hot-loop CPU win in transport.
  event_tx: arc_swap::ArcSwapOption<broadcast::Sender<Arc<serde_json::Value>>>,
  /// Routed event channels keyed by exact CDP method, for listeners
  /// that should not wake up for unrelated traffic.
  method_event_txs: Arc<DashMap<&'static str, broadcast::Sender<Arc<serde_json::Value>>>>,
  /// Routed event channels keyed by CDP domain (`Network`, `Fetch`,
  /// `Runtime`, ...), for listeners that legitimately consume many
  /// methods in one domain but should not receive the rest of CDP.
  domain_event_txs: Arc<DashMap<&'static str, broadcast::Sender<Arc<serde_json::Value>>>>,
  /// Lossless per-consumer taps keyed by exact CDP method. Fed
  /// synchronously by the reader task in wire order; the unbounded
  /// senders never drop, so tap consumers are structurally immune to
  /// the `Lagged` loss that broadcast subscribers tolerate.
  method_taps: Arc<DashMap<&'static str, Vec<EventTap>>>,
  /// Lossless per-consumer taps keyed by CDP domain.
  domain_taps: Arc<DashMap<&'static str, Vec<EventTap>>>,
  /// Lossless wildcard taps — every event of one exact session, in wire
  /// order. Backs the public `CDPSession` event stream.
  wildcard_taps: Arc<std::sync::Mutex<Vec<EventTap>>>,
  /// Per-method RTT statistics. Only populated when
  /// `FERRIDRIVER_RTT_STATS=1` is set; otherwise the entry insert /
  /// remove path skips the bookkeeping for zero-cost-when-unused.
  rtt_stats: Arc<std::sync::Mutex<RttStats>>,
}

/// Lock a `std::sync::Mutex`, recovering from poisoning.
///
/// `std::sync::Mutex` only fails when a thread panicked while holding the lock.
/// In the CDP dispatcher this is non-fatal -- we recover the inner data and continue.
fn lock_or_recover<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One registered lossless tap: an unbounded sender plus the session it
/// is scoped to (`None` = every session).
pub(crate) struct EventTap {
  session: Option<String>,
  tx: tokio::sync::mpsc::UnboundedSender<Arc<serde_json::Value>>,
  /// Strict taps match the event's `sessionId` exactly — browser-scoped
  /// events (empty sid) do NOT pass a page-session filter. Used by the
  /// public `CDPSession` wildcard stream, where an attached session must
  /// only observe its own events (Playwright `CRSession` semantics).
  strict: bool,
}

impl EventTap {
  /// Whether an event carrying `sid` (empty when the event is
  /// browser-scoped) belongs to this tap. For non-strict taps,
  /// browser-scoped events pass every session filter — per-page
  /// consumers of browser-level domains (`Browser.download*`) decide
  /// relevance themselves.
  fn matches_session(&self, sid: &str) -> bool {
    if self.strict {
      return self.session.as_deref() == Some(sid);
    }
    self.session.as_deref().is_none_or(|s| sid.is_empty() || sid == s)
  }
}

/// Deliver `msg` to every matching tap in `taps`, pruning taps whose
/// receiver was dropped.
fn send_to_taps(taps: &mut Vec<EventTap>, sid: &str, msg: &Arc<serde_json::Value>) {
  taps.retain(|tap| {
    if !tap.matches_session(sid) {
      return true;
    }
    tap.tx.send(Arc::clone(msg)).is_ok()
  });
}

/// Broadcast capacity for the per-transport event channel.
///
/// Every event subscriber (frame-cache listener, console drain,
/// network tracker, file-chooser listener, screencast tap, NAPI
/// `page.on(...)` registrations, ...) shares this single fan-out
/// queue. A slow subscriber that lags behind the producer makes
/// `tokio::sync::broadcast` drop the oldest queued message and
/// surface `RecvError::Lagged` to that subscriber. The frame
/// listener cannot recover from a dropped `Page.frameNavigated` —
/// the page's frame cache stays stale, and every subsequent
/// `locator(...)` waits for an element on the wrong frame.
///
/// 4096 is large enough to absorb a worst-case page load
/// (network requests + lifecycle + DOM events) for multiple
/// concurrent subscribers without dropping events. The memory
/// cost is bounded by `Arc<serde_json::Value>` * capacity per
/// transport, i.e. <1MB even at full saturation.
const EVENT_BROADCAST_CAPACITY: usize = 4096;

#[derive(Default)]
struct IframeTargets {
  attached: Vec<Arc<serde_json::Value>>,
  taps: Vec<tokio::sync::mpsc::UnboundedSender<IframeTargetEvent>>,
}

pub enum IframeTargetEvent {
  Target(Arc<serde_json::Value>),
  Checkpoint(tokio::sync::oneshot::Sender<()>),
  Closed,
}

pub struct IframeTargetSubscription {
  pub events: tokio::sync::mpsc::UnboundedReceiver<IframeTargetEvent>,
  pub checkpoints: tokio::sync::mpsc::UnboundedSender<IframeTargetEvent>,
}

impl IframeTargets {
  fn dispatch(&mut self, event: &Arc<serde_json::Value>) {
    let session = &event["params"]["sessionId"];
    if event["method"] == "Target.attachedToTarget" {
      if event["params"]["targetInfo"]["type"] != "iframe" {
        return;
      }
      self.attached.retain(|old| old["params"]["sessionId"] != *session);
      self.attached.push(event.clone());
    } else if let Some(session) = session.as_str() {
      self.forget_session(session);
    }
    self
      .taps
      .retain(|tap| tap.send(IframeTargetEvent::Target(event.clone())).is_ok());
  }

  fn forget_session(&mut self, session: &str) {
    let mut removed = vec![session.to_owned()];
    while let Some(session) = removed.pop() {
      self.attached.retain(|event| {
        if event["sessionId"] == session {
          if let Some(child) = event["params"]["sessionId"].as_str() {
            removed.push(child.to_owned());
          }
          false
        } else {
          event["params"]["sessionId"] != session
        }
      });
    }
  }
}

impl CdpDispatcher {
  pub fn new() -> Self {
    let (event_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
    Self {
      disconnected: std::sync::atomic::AtomicBool::new(false),
      next_id: AtomicU64::new(1),
      pending: Arc::new(DashMap::default()),
      lifecycle_trackers: Arc::new(DashMap::default()),
      iframe_targets: std::sync::Mutex::default(),
      context_snapshots: std::sync::Mutex::default(),
      event_tx: arc_swap::ArcSwapOption::from(Some(Arc::new(event_tx))),
      method_event_txs: Arc::new(DashMap::default()),
      domain_event_txs: Arc::new(DashMap::default()),
      method_taps: Arc::new(DashMap::default()),
      domain_taps: Arc::new(DashMap::default()),
      wildcard_taps: Arc::new(std::sync::Mutex::new(Vec::new())),
      rtt_stats: Arc::new(std::sync::Mutex::new(RttStats::default())),
    }
  }

  pub fn tap_iframe_targets(&self) -> IframeTargetSubscription {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut targets = lock_or_recover(&self.iframe_targets);
    if self.is_disconnected() {
      let _ = sender.send(IframeTargetEvent::Closed);
    } else {
      for event in &targets.attached {
        let _ = sender.send(IframeTargetEvent::Target(event.clone()));
      }
      targets.taps.push(sender.clone());
    }
    IframeTargetSubscription {
      events: receiver,
      checkpoints: sender,
    }
  }

  pub fn tap_event_methods(
    &self,
    methods: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for method in methods {
      self.method_taps.entry(method).or_default().push(EventTap {
        session: session_id.map(str::to_string),
        tx: tx.clone(),
        strict: false,
      });
    }
    if self.is_disconnected() {
      self.method_taps.clear();
    }
    rx
  }

  /// Lossless, wire-ordered tap of EVERY event carried by exactly one
  /// session (strict match — browser-scoped events do not leak into a
  /// page-session stream). Backs the public `CDPSession` event surface.
  pub fn tap_all_events(&self, session_id: &str) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut taps = lock_or_recover(&self.wildcard_taps);
    if !self.is_disconnected() {
      taps.push(EventTap {
        session: Some(session_id.to_string()),
        tx,
        strict: true,
      });
    }
    rx
  }

  pub fn tap_event_domains(
    &self,
    domains: &'static [&'static str],
    session_id: Option<&str>,
  ) -> tokio::sync::mpsc::UnboundedReceiver<Arc<serde_json::Value>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for domain in domains {
      self.domain_taps.entry(domain).or_default().push(EventTap {
        session: session_id.map(str::to_string),
        tx: tx.clone(),
        strict: false,
      });
    }
    if self.is_disconnected() {
      self.domain_taps.clear();
    }
    rx
  }

  pub fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
    frame_observer: super::transport::FrameStateObserver,
  ) {
    // Replay and registration share the event lock so live events cannot overtake the snapshot.
    let snapshots = lock_or_recover(&self.context_snapshots);
    if let Some(contexts) = snapshots.0.get(session_id) {
      for context in contexts.values() {
        frame_observer(&context.event, "Runtime.executionContextCreated");
      }
    }
    self.lifecycle_trackers.insert(
      session_id.to_string(),
      LifecycleTracker {
        state,
        notify,
        frame_observer,
      },
    );
    if self.is_disconnected() {
      self.lifecycle_trackers.clear();
    }
  }

  /// Drop the lifecycle tracker and every tap belonging to
  /// `session_id`. See [`CdpTransport::unregister_session`].
  pub fn unregister_session(&self, session_id: &str) {
    if session_id.is_empty() {
      return;
    }
    self.lifecycle_trackers.remove(session_id);
    lock_or_recover(&self.iframe_targets).forget_session(session_id);
    let drop_session = |taps: &mut Vec<EventTap>| taps.retain(|t| t.session.as_deref() != Some(session_id));
    for mut entry in self.method_taps.iter_mut() {
      drop_session(entry.value_mut());
    }
    for mut entry in self.domain_taps.iter_mut() {
      drop_session(entry.value_mut());
    }
    drop_session(
      &mut self
        .wildcard_taps
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
  }

  pub fn subscribe_events(&self) -> broadcast::Receiver<Arc<serde_json::Value>> {
    self
      .event_tx
      .load()
      .as_ref()
      .map_or_else(|| broadcast::channel(1).1, |sender| sender.subscribe())
  }

  pub fn subscribe_event_method(&self, method: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    let receiver = match self.method_event_txs.entry(method) {
      Entry::Occupied(entry) => entry.get().subscribe(),
      Entry::Vacant(entry) => {
        let (tx, rx) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        entry.insert(tx);
        rx
      },
    };
    if self.is_disconnected() {
      self.method_event_txs.clear();
    }
    receiver
  }

  pub fn subscribe_event_domain(&self, domain: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    let receiver = match self.domain_event_txs.entry(domain) {
      Entry::Occupied(entry) => entry.get().subscribe(),
      Entry::Vacant(entry) => {
        let (tx, rx) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        entry.insert(tx);
        rx
      },
    };
    if self.is_disconnected() {
      self.domain_event_txs.clear();
    }
    receiver
  }

  /// Drain every in-flight `send_command` oneshot and deliver a
  /// `target_closed` error. Called by the reader task on EOF / error
  /// so callers don't block on responses that will never arrive.
  /// Whether the connection to the browser has ended.
  ///
  /// Set once by the reader task on EOF. A browser that crashed or was
  /// OOM-killed leaves its instance entry behind otherwise, and every
  /// later session routed to that name gets `TargetClosed` forever
  /// instead of a fresh browser.
  pub fn is_disconnected(&self) -> bool {
    self.disconnected.load(std::sync::atomic::Ordering::Acquire)
  }

  pub fn fail_all_pending(&self, reason: &str) {
    self.disconnected.store(true, std::sync::atomic::Ordering::Release);
    self.event_tx.store(None);
    self.method_event_txs.clear();
    self.domain_event_txs.clear();
    self.method_taps.clear();
    self.domain_taps.clear();
    lock_or_recover(&self.wildcard_taps).clear();
    lock_or_recover(&self.context_snapshots).0.clear();
    self.lifecycle_trackers.clear();
    {
      let mut targets = lock_or_recover(&self.iframe_targets);
      targets.attached.clear();
      for tap in &targets.taps {
        let _ = tap.send(IframeTargetEvent::Closed);
      }
      targets.taps.clear();
    }
    // `DashMap::iter_mut` would hold shard locks; collect keys first.
    let keys: Vec<u64> = self.pending.iter().map(|e| *e.key()).collect();
    for id in keys {
      if let Some((_, entry)) = self.pending.remove(&id) {
        let _ = entry.tx.send(Err(FerriError::target_closed(Some(reason.to_string()))));
      }
    }
  }

  pub(crate) fn pending_command(&self, id: u64) -> PendingCommand<'_> {
    PendingCommand {
      dispatcher: self,
      id: Some(id),
    }
  }

  #[cfg(test)]
  pub(crate) fn pending_count(&self) -> usize {
    self.pending.len()
  }

  /// Drop the in-flight entry for `id` after a send failure or response
  /// timeout. Without this the entry lives in the map until transport
  /// teardown — a browser that stops responding but keeps the pipe open
  /// grows the map by one entry per retried command.
  pub fn forget_pending(&self, id: u64) {
    self.pending.remove(&id);
  }

  fn fail_session_pending(&self, detached_session: &[u8]) {
    if let Ok(session) = std::str::from_utf8(detached_session) {
      lock_or_recover(&self.context_snapshots).0.remove(session);
    }
    let requests: Vec<u64> = self
      .pending
      .iter()
      .filter(|entry| {
        entry
          .session_id
          .as_deref()
          .is_some_and(|id| id.as_bytes() == detached_session)
      })
      .map(|entry| *entry.key())
      .collect();
    for id in requests {
      if let Some((_, entry)) = self.pending.remove(&id) {
        let _ = entry
          .tx
          .send(Err(FerriError::target_closed(Some("CDP session detached".into()))));
      }
    }
  }

  /// Build a CDP command as NUL-terminated JSON bytes and register a response receiver.
  ///
  /// Fails immediately once the transport is known to be dead. Without
  /// that check a command issued after the browser exited is queued to a
  /// writer task that has already stopped reading, and the caller waits
  /// the full 30s response timeout for an answer that can never come —
  /// which is exactly what every call did after an external browser kill.
  pub fn build_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<(u64, Vec<u8>, oneshot::Receiver<CdpResult>)> {
    if self.is_disconnected() {
      return Err(FerriError::target_closed(Some(format!(
        "browser connection is closed (sending {method})"
      ))));
    }
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let params_str = serde_json::to_string(params).map_err(|e| FerriError::Backend(format!("Serialize: {e}")))?;
    let mut data = if let Some(sid) = session_id {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str},"sessionId":"{sid}"}}"#).into_bytes()
    } else {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#).into_bytes()
    };
    data.push(0);

    tracing::debug!(
      target: "ferridriver::cdp::send",
      id,
      method,
      params = truncate_for_log(&params_str, 200),
      "CDP >>",
    );

    let rx = self.register_command(id, session_id, method)?;
    Ok((id, data, rx))
  }

  fn register_command(&self, id: u64, session_id: Option<&str>, method: &str) -> Result<oneshot::Receiver<CdpResult>> {
    let (tx, rx) = oneshot::channel();
    let stats_enabled = rtt_stats_enabled();
    let entry = PendingEntry {
      tx,
      session_id: session_id.map(Into::into),
      // Allocate the method String only when stats are enabled —
      // saves the per-command alloc when stats are off (the common
      // path).
      method: if stats_enabled {
        method.to_string()
      } else {
        String::new()
      },
      send_at: stats_enabled.then(Instant::now),
    };
    self.pending.insert(id, entry);
    if self.is_disconnected() {
      self.pending.remove(&id);
      return Err(FerriError::target_closed(Some(format!(
        "browser connection closed while registering {method}"
      ))));
    }
    Ok(rx)
  }

  /// Dispatch a raw CDP message (response or event). Called by the reader task.
  pub fn dispatch_message(&self, raw: &[u8]) {
    if self.is_disconnected() {
      return;
    }
    let id = json_scan::json_id(raw);

    if id > 0 {
      // Response
      let error_field = json_scan::json_field(raw, b"error");
      let payload = if error_field.is_empty() {
        let result_field = json_scan::json_field(raw, b"result");
        if result_field.is_empty() {
          Ok(serde_json::Value::Object(serde_json::Map::new()))
        } else {
          let val: serde_json::Value =
            serde_json::from_slice(result_field).unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
          Ok(val)
        }
      } else {
        let msg_bytes = json_scan::error_message(error_field);
        let msg_str = std::str::from_utf8(msg_bytes).unwrap_or("CDP error");
        if msg_str == "Session with given id not found." {
          Err(FerriError::target_closed(Some(msg_str.to_owned())))
        } else {
          Err(FerriError::protocol("CDP", msg_str))
        }
      };
      tracing::debug!(
        target: "ferridriver::cdp::recv",
        id,
        ok = payload.is_ok(),
        payload = truncate_for_log(&format!("{payload:?}"), 200),
        "CDP << response",
      );
      if let Some((_, entry)) = self.pending.remove(&id) {
        if rtt_stats_enabled()
          && let Some(send_at) = entry.send_at
        {
          let elapsed = send_at.elapsed().as_nanos();
          // Record into both the per-dispatcher bucket (dump on
          // graceful Drop) AND the process-global bucket (dump via
          // libc atexit — covers `process::exit` paths where Drop
          // never runs because reader/writer tokio tasks still hold
          // dispatcher Arc clones).
          lock_or_recover(&self.rtt_stats).record(&entry.method, elapsed);
          lock_or_recover(global_rtt_stats()).record(&entry.method, elapsed);
        }
        let _ = entry.tx.send(payload);
      }
    } else {
      self.dispatch_event(raw);
    }
  }

  fn dispatch_event(&self, raw: &[u8]) {
    // Event
    let method = json_scan::json_string(json_scan::json_field(raw, b"method"));
    let session_id = json_scan::json_string(json_scan::json_field(raw, b"sessionId"));
    let method_str = std::str::from_utf8(method).unwrap_or("");
    let sid_str = std::str::from_utf8(session_id).unwrap_or("");

    let detached_session = match method_str {
      "Inspector.detached" => session_id,
      "Target.detachedFromTarget" => {
        let params = json_scan::json_field(raw, b"params");
        json_scan::json_string(json_scan::json_field(params, b"sessionId"))
      },
      _ => b"",
    };
    if !detached_session.is_empty() {
      self.fail_session_pending(detached_session);
    }

    if matches!(
      method_str,
      "Runtime.executionContextCreated"
        | "Runtime.executionContextDestroyed"
        | "Runtime.executionContextsCleared"
        | "Runtime.bindingCalled"
        | "Page.frameDetached"
    ) {
      let mut snapshots = lock_or_recover(&self.context_snapshots);
      if method_str != "Runtime.bindingCalled" {
        snapshots.observe(sid_str, method_str, raw);
      }
      self.dispatch_lifecycle(raw, method_str, sid_str);
    } else {
      self.dispatch_lifecycle(raw, method_str, sid_str);
    }

    tracing::trace!(
      target: "ferridriver::cdp::recv",
      method = method_str,
      "CDP << event",
    );

    let domain = method_str.split_once('.').map(|(domain, _)| domain);
    let method_tx = self.method_event_txs.get(method_str).map(|entry| entry.clone());
    let domain_tx = domain.and_then(|d| self.domain_event_txs.get(d).map(|entry| entry.clone()));
    let global = self.event_tx.load();
    let needs_global = global.as_ref().is_some_and(|sender| sender.receiver_count() > 0);
    let needs_method = method_tx.as_ref().is_some_and(|tx| tx.receiver_count() > 0);
    let needs_domain = domain_tx.as_ref().is_some_and(|tx| tx.receiver_count() > 0);
    let has_method_taps = self.method_taps.get(method_str).is_some_and(|taps| !taps.is_empty());
    let has_domain_taps = domain.is_some_and(|d| self.domain_taps.get(d).is_some_and(|taps| !taps.is_empty()));
    let has_wildcard_taps = !lock_or_recover(&self.wildcard_taps).is_empty();
    let target_event = matches!(method_str, "Target.attachedToTarget" | "Target.detachedFromTarget");

    if (target_event
      || needs_global
      || needs_method
      || needs_domain
      || has_method_taps
      || has_domain_taps
      || has_wildcard_taps)
      && let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw)
    {
      let msg = Arc::new(msg);
      if target_event {
        lock_or_recover(&self.iframe_targets).dispatch(&msg);
      }
      // Taps first: state trackers must see the event before any
      // best-effort broadcast consumer can react to it.
      if has_method_taps && let Some(mut taps) = self.method_taps.get_mut(method_str) {
        send_to_taps(&mut taps, sid_str, &msg);
      }
      if has_domain_taps
        && let Some(d) = domain
        && let Some(mut taps) = self.domain_taps.get_mut(d)
      {
        send_to_taps(&mut taps, sid_str, &msg);
      }
      if has_wildcard_taps {
        send_to_taps(&mut lock_or_recover(&self.wildcard_taps), sid_str, &msg);
      }
      if needs_global && let Some(sender) = global.as_ref() {
        let _ = sender.send(msg.clone());
      }
      if needs_method && let Some(tx) = method_tx {
        let _ = tx.send(msg.clone());
      }
      if needs_domain && let Some(tx) = domain_tx {
        let _ = tx.send(msg);
      }
    }
  }

  /// Lifecycle tracker dispatch -- tracks `loaderId` for document-accurate
  /// lifecycle. Only main-frame events update the tracker: a subframe's
  /// `Page.frameNavigated` carries a `parentId` (mirrors Playwright's
  /// `_eventBelongsToStaleFrame` filter — main-frame and subframe nav
  /// states have independent lifecycle in
  /// `/tmp/playwright/packages/playwright-core/src/server/frames.ts`),
  /// and a subframe's `Page.lifecycleEvent` carries a `loaderId` that
  /// will not match the main frame's `current_loader_id`.
  ///
  /// `Inspector.targetCrashed` sets `crashed` and wakes waiters so
  /// goto/reload return immediately instead of stalling until timeout.
  fn dispatch_lifecycle(&self, raw: &[u8], method_str: &str, key: &str) {
    if let Some(tracker) = self.lifecycle_trackers.get(key) {
      (tracker.frame_observer)(raw, method_str);
      match method_str {
        "Page.frameNavigated" => {
          let params = json_scan::json_field(raw, b"params");
          let frame = json_scan::json_field(params, b"frame");
          let parent_id = json_scan::json_field(frame, b"parentId");
          if !parent_id.is_empty() {
            // Subframe commit — leave main-frame lifecycle untouched.
            return;
          }
          let loader_id = json_scan::json_string(json_scan::json_field(frame, b"loaderId"));
          // Allocate the owned loaderId string BEFORE taking the lock so the
          // critical section holds no heap work — only the two field stores.
          let loader_id_owned = std::str::from_utf8(loader_id).unwrap_or("").to_string();
          {
            let mut state = lock_or_recover(&tracker.state);
            state.current_loader_id = loader_id_owned;
            state.fired = super::LC_COMMIT;
            state.nav_committed_seq = state.nav_committed_seq.wrapping_add(1);
          }
          tracker.notify.notify_waiters();
        },
        "Page.frameStartedNavigating" => {
          // A navigation began (any frame). Bump the generation counter so a
          // post-input settle knows the action created a navigation and must
          // wait for it to commit. Main-frame filtering is done by the waiter
          // (it waits for the main-frame loader to change, bounded), so a
          // subframe nav here costs at most the bounded settle, never a hang.
          {
            let mut state = lock_or_recover(&tracker.state);
            state.nav_started_seq = state.nav_started_seq.wrapping_add(1);
          }
          tracker.notify.notify_waiters();
        },
        "Page.lifecycleEvent" => {
          let params = json_scan::json_field(raw, b"params");
          let loader_id = json_scan::json_string(json_scan::json_field(params, b"loaderId"));
          let loader_id_str = std::str::from_utf8(loader_id).unwrap_or("");
          let name = json_scan::json_string(json_scan::json_field(params, b"name"));
          let name_str = std::str::from_utf8(name).unwrap_or("");
          let event_name = match name_str {
            "DOMContentLoaded" => Some(super::LC_DOMCONTENTLOADED),
            "load" => Some(super::LC_LOAD),
            _ => None,
          };
          if let Some(event_flag) = event_name {
            // Strict loaderId match. A subframe lifecycle event carries
            // the subframe's loaderId, which never matches the main
            // frame's `current_loader_id`. The previous "or is_empty()"
            // relaxation was a workaround for the initial-nav case
            // where `current_loader_id` is unset before the first
            // `Page.frameNavigated` lands; we drop it because the
            // wrapper now seeds `current_loader_id` from the
            // `Page.navigate` response (see
            // `crates/ferridriver/src/backend/cdp/mod.rs::CdpPage::goto`)
            // before awaiting any lifecycle event.
            //
            // The critical section holds only the compare-and-set; the
            // `loader_id_str` borrows the incoming buffer (no allocation),
            // and `notify_waiters` runs after the guard drops.
            let matched = {
              let mut state = lock_or_recover(&tracker.state);
              if state.current_loader_id == loader_id_str {
                state.fired |= event_flag;
                true
              } else {
                false
              }
            };
            if matched {
              tracker.notify.notify_waiters();
            }
          }
        },
        "Inspector.targetCrashed" => {
          let mut state = lock_or_recover(&tracker.state);
          state.crashed = true;
          drop(state);
          tracker.notify.notify_waiters();
        },
        _ => {},
      }
    }
  }
}

pub(super) fn navigated_frame(frame: &serde_json::Value) -> super::super::FrameInfo {
  let text = |key: &str| frame.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
  super::super::FrameInfo {
    frame_id: text("id").to_string(),
    parent_frame_id: frame
      .get("parentId")
      .and_then(serde_json::Value::as_str)
      .map(str::to_owned),
    name: text("name").to_string(),
    url: format!("{}{}", text("url"), text("urlFragment")),
  }
}

fn frame_event(raw: &[u8], method: &str) -> Option<crate::events::PageEvent> {
  use crate::events::PageEvent;
  if !matches!(
    method,
    "Page.frameAttached" | "Page.frameDetached" | "Page.frameNavigated" | "Page.navigatedWithinDocument"
  ) {
    return None;
  }
  let params: serde_json::Value = serde_json::from_slice(json_scan::json_field(raw, b"params")).ok()?;
  let text = |key: &str| {
    params
      .get(key)
      .and_then(serde_json::Value::as_str)
      .unwrap_or("")
      .to_string()
  };
  match method {
    "Page.frameAttached" => Some(PageEvent::FrameAttached(super::super::FrameInfo {
      frame_id: text("frameId"),
      parent_frame_id: params
        .get("parentFrameId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned),
      name: String::new(),
      url: String::new(),
    })),
    "Page.frameDetached" => Some(PageEvent::FrameDetached {
      frame_id: text("frameId"),
    }),
    "Page.frameNavigated" => Some(PageEvent::FrameNavigated(navigated_frame(params.get("frame")?))),
    "Page.navigatedWithinDocument" => Some(PageEvent::FrameNavigatedWithinDocument(super::super::FrameInfo {
      frame_id: text("frameId"),
      parent_frame_id: None,
      name: String::new(),
      url: text("url"),
    })),
    _ => None,
  }
}

impl Drop for CdpDispatcher {
  fn drop(&mut self) {
    // Dump per-method RTT stats on transport teardown when stats
    // collection is enabled. Catches clean shutdowns where the
    // dispatcher Arc reaches zero before process exit. NAPI / cargo
    // test paths typically exit via `process::exit()` while reader /
    // writer tokio tasks still hold dispatcher clones — for those,
    // [`global_rtt_stats`] aggregates per-dispatcher buckets and is
    // dumped via the libc atexit hook registered in
    // [`rtt_stats_enabled`].
    if rtt_stats_enabled() {
      let local = lock_or_recover(&self.rtt_stats);
      lock_or_recover(global_rtt_stats()).merge(&local);
      local.dump();
    }
  }
}

#[cfg(test)]
pub(crate) async fn verify_operation_budgets<T: CdpTransport, M>(
  transport: T,
  dispatcher: Arc<CdpDispatcher>,
  writer: tokio::sync::mpsc::Sender<M>,
  mut queued: tokio::sync::mpsc::Receiver<M>,
  placeholder: M,
) {
  use crate::operation_budget::OperationBudget;
  let params = serde_json::json!({});
  for timeout in [0, 90_000] {
    let mut command = Box::pin(OperationBudget::new(timeout).unwrap().scope(transport.send_command(
      None,
      "Runtime.evaluate",
      &params,
    )));
    assert!(futures::poll!(&mut command).is_pending());
    assert!(queued.recv().await.is_some());
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    assert!(futures::poll!(&mut command).is_pending());
    assert_eq!(dispatcher.pending_count(), 1);
    let id = *dispatcher.pending.iter().next().unwrap().key();
    dispatcher.dispatch_message(
      serde_json::json!({"id":id,"result":{"value":42}})
        .to_string()
        .as_bytes(),
    );
    assert_eq!(command.await.unwrap()["value"], 42);
    assert_eq!(dispatcher.pending_count(), 0);
  }
  let mut command = Box::pin(transport.send_command(None, "Runtime.evaluate", &params));
  assert!(futures::poll!(&mut command).is_pending());
  assert!(queued.recv().await.is_some());
  let id = *dispatcher.pending.iter().next().unwrap().key();
  drop(command);
  assert_eq!(dispatcher.pending_count(), 0);
  dispatcher.dispatch_message(serde_json::json!({"id":id,"result":{}}).to_string().as_bytes());
  assert_eq!(dispatcher.pending_count(), 0);
  writer
    .send(placeholder)
    .await
    .unwrap_or_else(|_| panic!("writer closed"));
  let mut command = Box::pin(OperationBudget::new(1000).unwrap().scope(transport.send_command(
    None,
    "Runtime.evaluate",
    &params,
  )));
  assert!(futures::poll!(&mut command).is_pending());
  tokio::time::advance(std::time::Duration::from_secs(1)).await;
  assert!(command.await.unwrap_err().is_timeout_error());
  assert_eq!(dispatcher.pending_count(), 0);
  assert!(queued.recv().await.is_some());
  assert!(queued.try_recv().is_err());
}

#[cfg(test)]
mod tests {
  #[test]
  fn retired_contexts_are_not_replayed_to_later_observers() {
    use serde_json::json;
    for retired in [
      json!({"sessionId":"session","method":"Runtime.executionContextDestroyed","params":{"executionContextId":11}}),
      json!({"sessionId":"session","method":"Runtime.executionContextsCleared","params":{}}),
      json!({"sessionId":"session","method":"Page.frameDetached","params":{"frameId":"frame"}}),
      json!({"method":"Target.detachedFromTarget","params":{"sessionId":"session"}}),
      json!({"sessionId":"session","method":"Inspector.detached","params":{}}),
      serde_json::Value::Null,
    ] {
      let dispatcher = super::CdpDispatcher::new();
      dispatcher.dispatch_message(
        &serde_json::to_vec(
          &json!({"sessionId":"session","method":"Runtime.executionContextCreated",
        "params":{"context":{"id":11,"auxData":{"frameId":"frame","isDefault":true}}}}),
        )
        .unwrap(),
      );
      if retired.is_null() {
        dispatcher.fail_all_pending("fixture closed");
      } else {
        dispatcher.dispatch_message(&serde_json::to_vec(&retired).unwrap());
      }
      let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
      let observed = seen.clone();
      dispatcher.register_lifecycle_tracker(
        "session",
        std::sync::Arc::new(std::sync::Mutex::new(super::super::LifecycleState::new())),
        std::sync::Arc::new(tokio::sync::Notify::new()),
        std::sync::Arc::new(move |_, method| {
          observed.lock().unwrap().push(method.to_owned());
        }),
      );
      assert!(
        seen.lock().unwrap().is_empty(),
        "retired context was replayed after {retired}"
      );
    }
  }

  #[test]
  fn closing_terminates_existing_and_late_event_subscriptions() {
    let dispatcher = super::CdpDispatcher::new();
    for _ in 0..2 {
      let broadcasts = [
        dispatcher.subscribe_events(),
        dispatcher.subscribe_event_method("Page.loadEventFired"),
        dispatcher.subscribe_event_domain("Page"),
      ];
      let taps = [
        dispatcher.tap_event_methods(&["Page.loadEventFired"], None),
        dispatcher.tap_event_domains(&["Page"], None),
        dispatcher.tap_all_events("sashoush"),
      ];
      dispatcher.fail_all_pending("closed");
      dispatcher.dispatch_message(br#"{"method":"Page.loadEventFired","sessionId":"sashoush","params":{}}"#);
      for mut receiver in broadcasts {
        assert!(matches!(
          receiver.try_recv(),
          Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
      }
      for mut receiver in taps {
        assert!(matches!(
          receiver.try_recv(),
          Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
      }
    }
  }

  #[test]
  fn closing_during_command_encoding_cannot_leave_a_pending_response() {
    let dispatcher = super::CdpDispatcher::new();
    dispatcher.fail_all_pending("closed during command encoding");
    let result = dispatcher.register_command(1, None, "Runtime.evaluate");
    assert!(matches!(result, Err(crate::FerriError::TargetClosed { .. })));
    assert_eq!(dispatcher.pending_count(), 0);
  }

  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Instant;

  #[test]
  fn detaching_one_session_rejects_only_its_pending_commands() {
    for event in [
      br#"{"method":"Inspector.detached","sessionId":"closing","params":{"reason":"target_closed"}}"#.as_slice(),
      br#"{"method":"Target.detachedFromTarget","params":{"sessionId":"closing","targetId":"page"}}"#.as_slice(),
    ] {
      let dispatcher = CdpDispatcher::new();
      let (_, _, mut closing) = dispatcher
        .build_command(Some("closing"), "Runtime.evaluate", &serde_json::json!({}))
        .unwrap();
      let (_, _, mut live) = dispatcher
        .build_command(Some("live"), "Runtime.evaluate", &serde_json::json!({}))
        .unwrap();
      let (_, _, mut root) = dispatcher
        .build_command(None, "Target.getTargets", &serde_json::json!({}))
        .unwrap();
      dispatcher.dispatch_message(event);
      assert!(matches!(closing.try_recv(), Ok(Err(FerriError::TargetClosed { .. }))));
      assert!(matches!(live.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
      assert!(matches!(root.try_recv(), Err(oneshot::error::TryRecvError::Empty)));
      assert_eq!(dispatcher.pending.len(), 2);
      dispatcher.fail_all_pending("test finished");
    }
  }

  #[test]
  fn commands_racing_session_detachment_report_target_closed() {
    let dispatcher = CdpDispatcher::new();
    let (id, _, mut response) = dispatcher
      .build_command(Some("closing"), "Runtime.evaluate", &serde_json::json!({}))
      .unwrap();
    dispatcher.dispatch_message(
      format!(r#"{{"id":{id},"error":{{"code":-32001,"message":"Session with given id not found."}}}}"#).as_bytes(),
    );
    assert!(matches!(response.try_recv(), Ok(Err(FerriError::TargetClosed { .. }))));
  }

  const NETWORK_EVENT: &[u8] = br#"{"method":"Network.requestWillBeSent","sessionId":"s1","params":{"requestId":"r1","request":{"url":"https://example.test/asset.js","method":"GET"},"type":"Script"}}"#;
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  #[ignore = "benchmark; run with --ignored --nocapture"]
  async fn bench_routed_event_dispatch_wakeups() {
    const EVENTS: usize = 2_000;
    const LISTENERS: usize = 8;

    let global = CdpDispatcher::new();
    let global_wakeups = Arc::new(AtomicUsize::new(0));
    let mut global_handles = Vec::with_capacity(LISTENERS);
    for _ in 0..LISTENERS {
      let mut rx = global.subscribe_events();
      let wakeups = global_wakeups.clone();
      global_handles.push(tokio::spawn(async move {
        for _ in 0..EVENTS {
          let event = rx.recv().await.expect("global event");
          let _ = event.get("method").and_then(|m| m.as_str());
          wakeups.fetch_add(1, Ordering::Relaxed);
        }
      }));
    }
    tokio::task::yield_now().await;
    let global_started = Instant::now();
    for _ in 0..EVENTS {
      global.dispatch_message(NETWORK_EVENT);
    }
    for handle in global_handles {
      handle.await.expect("global listener task");
    }
    let global_elapsed = global_started.elapsed();

    let routed = CdpDispatcher::new();
    let mut idle_method_receivers = [
      routed.subscribe_event_method("Runtime.consoleAPICalled"),
      routed.subscribe_event_method("Runtime.exceptionThrown"),
      routed.subscribe_event_method("Page.javascriptDialogOpening"),
      routed.subscribe_event_method("Page.fileChooserOpened"),
      routed.subscribe_event_method("Runtime.bindingCalled"),
      routed.subscribe_event_method("Page.screencastFrame"),
    ];
    let mut idle_domain_receivers = [
      routed.subscribe_event_domain("Browser"),
      routed.subscribe_event_domain("Fetch"),
    ];
    let routed_wakeups = Arc::new(AtomicUsize::new(0));
    let mut network_rx = routed.subscribe_event_domain("Network");
    let wakeups = routed_wakeups.clone();
    let network_handle = tokio::spawn(async move {
      for _ in 0..EVENTS {
        let event = network_rx.recv().await.expect("routed network event");
        let _ = event.get("method").and_then(|m| m.as_str());
        wakeups.fetch_add(1, Ordering::Relaxed);
      }
    });
    tokio::task::yield_now().await;
    let routed_started = Instant::now();
    for _ in 0..EVENTS {
      routed.dispatch_message(NETWORK_EVENT);
    }
    network_handle.await.expect("network listener task");
    let routed_elapsed = routed_started.elapsed();

    let idle_method_wakeups: usize = idle_method_receivers
      .iter_mut()
      .map(|rx| rx.try_recv().ok().map_or(0, |_| 1))
      .sum();
    let idle_domain_wakeups: usize = idle_domain_receivers
      .iter_mut()
      .map(|rx| rx.try_recv().ok().map_or(0, |_| 1))
      .sum();

    println!(
      "global broadcast: {:?}, wakeups={}",
      global_elapsed,
      global_wakeups.load(Ordering::Relaxed)
    );
    println!(
      "routed dispatch:   {:?}, wakeups={}, idle_wakeups={}",
      routed_elapsed,
      routed_wakeups.load(Ordering::Relaxed),
      idle_method_wakeups + idle_domain_wakeups
    );

    assert_eq!(global_wakeups.load(Ordering::Relaxed), EVENTS * LISTENERS);
    assert_eq!(routed_wakeups.load(Ordering::Relaxed), EVENTS);
    assert_eq!(idle_method_wakeups + idle_domain_wakeups, 0);
  }

  /// `build_command` borrows its params, so the shared `EMPTY_PARAMS`
  /// static can be serialized by reference for every no-param CDP call
  /// instead of cloning a fresh `{}` map per call. This guards both the
  /// borrowed signature and the serialized wire shape: `params:{}` with
  /// no trailing junk.
  #[test]
  fn build_command_serializes_borrowed_empty_params_by_reference() {
    let dispatcher = CdpDispatcher::new();
    let empty = &crate::backend::EMPTY_PARAMS;

    // Two builds from the same static reference: identical params shape,
    // monotonically increasing ids, and no per-call clone of the map
    // (the reference is what reaches `build_command`).
    let (_id1, data1, _rx1) = dispatcher
      .build_command(Some("sess-1"), "Page.enable", empty)
      .expect("build with session");
    let (_id2, data2, _rx2) = dispatcher
      .build_command(None, "Runtime.enable", empty)
      .expect("build without session");

    let s1 = String::from_utf8(data1).expect("utf8");
    let s2 = String::from_utf8(data2).expect("utf8");

    assert!(s1.contains(r#""params":{}"#), "expected empty params object, got {s1}");
    assert!(s1.contains(r#""method":"Page.enable""#), "method missing: {s1}");
    assert!(s1.contains(r#""sessionId":"sess-1""#), "sessionId missing: {s1}");
    assert!(s2.contains(r#""params":{}"#), "expected empty params object, got {s2}");
    assert!(!s2.contains("sessionId"), "no sessionId expected: {s2}");

    // The static is a single shared instance — repeated borrows resolve
    // to the same address, confirming no clone is materialized per call.
    let a = std::ptr::from_ref::<serde_json::Value>(&crate::backend::EMPTY_PARAMS);
    let b = std::ptr::from_ref::<serde_json::Value>(&crate::backend::EMPTY_PARAMS);
    assert_eq!(a, b, "EMPTY_PARAMS must be a single shared static");
  }

  fn register_test_tracker(
    dispatcher: &CdpDispatcher,
    session_id: &str,
  ) -> Arc<std::sync::Mutex<super::super::LifecycleState>> {
    let state = Arc::new(std::sync::Mutex::new(super::super::LifecycleState {
      current_loader_id: String::new(),
      nav_started_seq: 0,
      nav_committed_seq: 0,
      fired: 0,
      crashed: false,
    }));
    let notify = Arc::new(tokio::sync::Notify::new());
    dispatcher.register_lifecycle_tracker(
      session_id,
      state.clone(),
      notify,
      frame_state_observer(Arc::default(), crate::events::EventEmitter::new()),
    );
    state
  }

  #[test]
  fn frame_cache_is_current_when_lifecycle_waiters_are_released() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");
    let cache = Arc::default();
    dispatcher.register_lifecycle_tracker(
      "s1",
      state.clone(),
      Arc::default(),
      frame_state_observer(Arc::clone(&cache), crate::events::EventEmitter::new()),
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    assert_eq!(lock_or_recover(&state).fired, super::super::LC_COMMIT);
    assert_eq!(
      lock_or_recover(&cache).record("f1").unwrap().info.url,
      "https://example.test/"
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"other","params":{"frame":{"id":"f1","url":"https://wrong.test/"}}}"#,
    );
    assert_eq!(
      lock_or_recover(&cache).record("f1").unwrap().info.url,
      "https://example.test/"
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameAttached","sessionId":"s1","params":{"frameId":"child","parentFrameId":"f1"}}"#,
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"child","parentId":"f1","name":"child","url":"https://example.test/frame"}}}"#,
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.navigatedWithinDocument","sessionId":"s1","params":{"frameId":"child","url":"https://example.test/frame#\u0061"}}"#,
    );
    assert_eq!(
      lock_or_recover(&cache).record("child").unwrap().info.url,
      "https://example.test/frame#a"
    );
    assert_eq!(lock_or_recover(&state).current_loader_id, "L1");
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameAttached","sessionId":"s1","params":{"frameId":"grandchild","parentFrameId":"child"}}"#,
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameDetached","sessionId":"s1","params":{"frameId":"child","reason":"swap"}}"#,
    );
    assert!(!lock_or_recover(&cache).record("child").unwrap().detached);
    assert!(lock_or_recover(&cache).record("grandchild").unwrap().detached);
    dispatcher.register_lifecycle_tracker(
      "remote",
      Arc::new(std::sync::Mutex::new(super::super::LifecycleState::new())),
      Arc::default(),
      frame_state_observer_for_renderer(
        Arc::clone(&cache),
        crate::events::EventEmitter::new(),
        "child".into(),
        "f1".into(),
      ),
    );
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"remote","params":{"frame":{"id":"child","loaderId":"L2","name":"child","url":"https://remote.test/"}}}"#,
    );
    assert_eq!(lock_or_recover(&cache).parent_id("child").as_deref(), Some("f1"));
    assert!(!lock_or_recover(&cache).record("child").unwrap().detached);
    assert_eq!(lock_or_recover(&state).current_loader_id, "L1");
    dispatcher.dispatch_message(br#"{"method":"Page.frameDetached","sessionId":"s1","params":{"frameId":"child"}}"#);
    assert!(lock_or_recover(&cache).record("child").unwrap().detached);
  }

  #[test]
  fn iframe_target_subscriptions_replay_live_sessions_then_follow_wire_order() {
    let dispatcher = CdpDispatcher::new();
    dispatcher.dispatch_message(br#"{"method":"Target.attachedToTarget","sessionId":"parent","params":{"sessionId":"child","targetInfo":{"type":"iframe","targetId":"frame"}}}"#);
    let subscription = dispatcher.tap_iframe_targets();
    let mut events = subscription.events;
    let target = |event| match event {
      IframeTargetEvent::Target(value) => value,
      _ => panic!("expected target event"),
    };
    assert_eq!(target(events.try_recv().unwrap())["params"]["sessionId"], "child");
    assert!(events.try_recv().is_err());
    dispatcher.dispatch_message(br#"{"method":"Target.attachedToTarget","sessionId":"child","params":{"sessionId":"nested","targetInfo":{"type":"iframe","targetId":"nested-frame"}}}"#);
    assert_eq!(target(events.try_recv().unwrap())["params"]["sessionId"], "nested");
    dispatcher.dispatch_message(
      br#"{"method":"Target.detachedFromTarget","sessionId":"parent","params":{"sessionId":"child"}}"#,
    );
    assert_eq!(
      target(events.try_recv().unwrap())["method"],
      "Target.detachedFromTarget"
    );
    assert!(dispatcher.tap_iframe_targets().events.try_recv().is_err());
    dispatcher.fail_all_pending("test disconnect");
    assert!(matches!(events.try_recv(), Ok(IframeTargetEvent::Closed)));
  }

  #[test]
  fn lifecycle_dispatch_commit_then_load_updates_state() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    {
      let guard = lock_or_recover(&state);
      assert_eq!(guard.current_loader_id, "L1");
      assert_eq!(guard.fired, super::super::LC_COMMIT);
    }

    dispatcher.dispatch_message(
      br#"{"method":"Page.lifecycleEvent","sessionId":"s1","params":{"loaderId":"L1","name":"load"}}"#,
    );
    {
      let guard = lock_or_recover(&state);
      assert_eq!(guard.current_loader_id, "L1");
      assert_eq!(guard.fired, super::super::LC_COMMIT | super::super::LC_LOAD);
    }
  }

  #[test]
  fn lifecycle_dispatch_subframe_navigation_does_not_clobber_main() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    // Subframe commit carries a parentId; main-frame lifecycle must be untouched.
    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f2","parentId":"f1","loaderId":"L2","url":"https://sub.example.test/"}}}"#,
    );
    let guard = lock_or_recover(&state);
    assert_eq!(guard.current_loader_id, "L1");
    assert_eq!(guard.fired, super::super::LC_COMMIT);
  }

  #[test]
  fn lifecycle_dispatch_mismatched_loader_id_is_ignored() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(
      br#"{"method":"Page.frameNavigated","sessionId":"s1","params":{"frame":{"id":"f1","loaderId":"L1","url":"https://example.test/"}}}"#,
    );
    // A subframe lifecycle event carries a different loaderId; it must not set load.
    dispatcher.dispatch_message(
      br#"{"method":"Page.lifecycleEvent","sessionId":"s1","params":{"loaderId":"OTHER","name":"load"}}"#,
    );
    let guard = lock_or_recover(&state);
    assert_eq!(guard.fired, super::super::LC_COMMIT);
  }

  #[test]
  fn lifecycle_dispatch_target_crashed_sets_flag() {
    let dispatcher = CdpDispatcher::new();
    let state = register_test_tracker(&dispatcher, "s1");

    dispatcher.dispatch_message(br#"{"method":"Inspector.targetCrashed","sessionId":"s1","params":{}}"#);
    assert!(lock_or_recover(&state).crashed);
  }

  #[test]
  fn tap_is_lossless_and_wire_ordered_across_domains() {
    let dispatcher = CdpDispatcher::new();
    let mut rx = dispatcher.tap_event_domains(&["Runtime", "Page"], Some("s1"));

    for i in 0..5000 {
      let (method, sid) = match i % 3 {
        0 => ("Runtime.executionContextCreated", "s1"),
        1 => ("Page.frameNavigated", "s1"),
        // Other-session traffic must be filtered out.
        _ => ("Runtime.executionContextCreated", "s2"),
      };
      dispatcher
        .dispatch_message(format!(r#"{{"method":"{method}","sessionId":"{sid}","params":{{"seq":{i}}}}}"#).as_bytes());
    }

    let mut seqs = Vec::new();
    while let Ok(msg) = rx.try_recv() {
      assert_eq!(msg.get("sessionId").and_then(|v| v.as_str()), Some("s1"));
      seqs.push(
        msg
          .get("params")
          .and_then(|p| p.get("seq"))
          .and_then(serde_json::Value::as_i64)
          .unwrap_or(-1),
      );
    }
    let expected: Vec<i64> = (0..5000).filter(|i| i % 3 != 2).collect();
    assert_eq!(
      seqs, expected,
      "every own-session event delivered, in wire order, none dropped"
    );
  }

  #[test]
  fn tap_method_filter_and_browser_scoped_passthrough() {
    let dispatcher = CdpDispatcher::new();
    let mut rx = dispatcher.tap_event_methods(&["Browser.downloadWillBegin"], Some("s1"));

    // Browser-scoped event without sessionId passes a session-scoped tap.
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadWillBegin","params":{"guid":"g1"}}"#);
    // Matching session passes; foreign session and foreign method do not.
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadWillBegin","sessionId":"s1","params":{"guid":"g2"}}"#);
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadWillBegin","sessionId":"s2","params":{"guid":"g3"}}"#);
    dispatcher.dispatch_message(br#"{"method":"Browser.downloadProgress","sessionId":"s1","params":{"guid":"g4"}}"#);

    let mut guids = Vec::new();
    while let Ok(msg) = rx.try_recv() {
      guids.push(
        msg
          .get("params")
          .and_then(|p| p.get("guid"))
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string(),
      );
    }
    assert_eq!(guids, ["g1", "g2"]);
  }

  #[test]
  fn tap_pruned_after_receiver_drop() {
    let dispatcher = CdpDispatcher::new();
    let rx = dispatcher.tap_event_domains(&["Network"], None);
    drop(rx);
    dispatcher.dispatch_message(br#"{"method":"Network.requestWillBeSent","sessionId":"s1","params":{}}"#);
    let empty = dispatcher.domain_taps.get("Network").is_none_or(|taps| taps.is_empty());
    assert!(empty, "dead tap must be pruned on next dispatch");
  }
}
