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
  if let Ok(stats) = global_rtt_stats().lock() {
    if !stats.buckets.is_empty() {
      stats.dump();
    }
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
  if let Ok(stats) = global_rtt_stats().lock() {
    if !stats.buckets.is_empty() {
      stats.dump();
    }
  }
}

/// Trait abstracting CDP transport medium (pipes vs WebSocket).
pub trait CdpTransport: Send + Sync + 'static {
  fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: serde_json::Value,
  ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send;

  fn subscribe_events(&self) -> broadcast::Receiver<Arc<serde_json::Value>>;

  fn subscribe_event_method(&self, method: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>>;

  fn subscribe_event_domain(&self, domain: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>>;

  fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
  );
}

// ── Shared dispatch state ──────────────────────────────────────────────────

pub(crate) struct LifecycleTracker {
  pub state: Arc<std::sync::Mutex<super::LifecycleState>>,
  pub notify: Arc<tokio::sync::Notify>,
}

/// Shared CDP message dispatch state. Embedded by both `PipeTransport` and `WsTransport`.
pub(crate) struct CdpDispatcher {
  pub next_id: AtomicU64,
  pub pending: Arc<PendingMap>,
  /// Per-session lifecycle trackers (keyed by sessionId). Sharded
  /// via `DashMap` so events firing on N sessions don't contend on
  /// the same mutex.
  lifecycle_trackers: Arc<DashMap<String, LifecycleTracker>>,
  /// Per-message broadcast channel. Wraps the message in `Arc` so
  /// fanout to N subscribers is N refcount bumps (~5ns each)
  /// instead of N deep `serde_json::Value` clones (~400ns + ~10
  /// allocs each). At 200 events/s × ~12 subscribers per page this
  /// is the single biggest hot-loop CPU win in transport.
  pub event_tx: broadcast::Sender<Arc<serde_json::Value>>,
  /// Routed event channels keyed by exact CDP method, for listeners
  /// that should not wake up for unrelated traffic.
  method_event_txs: Arc<DashMap<&'static str, broadcast::Sender<Arc<serde_json::Value>>>>,
  /// Routed event channels keyed by CDP domain (`Network`, `Fetch`,
  /// `Runtime`, ...), for listeners that legitimately consume many
  /// methods in one domain but should not receive the rest of CDP.
  domain_event_txs: Arc<DashMap<&'static str, broadcast::Sender<Arc<serde_json::Value>>>>,
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

impl CdpDispatcher {
  pub fn new() -> Self {
    let (event_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
    Self {
      next_id: AtomicU64::new(1),
      pending: Arc::new(DashMap::default()),
      lifecycle_trackers: Arc::new(DashMap::default()),
      event_tx,
      method_event_txs: Arc::new(DashMap::default()),
      domain_event_txs: Arc::new(DashMap::default()),
      rtt_stats: Arc::new(std::sync::Mutex::new(RttStats::default())),
    }
  }

  pub fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
  ) {
    self
      .lifecycle_trackers
      .insert(session_id.to_string(), LifecycleTracker { state, notify });
  }

  pub fn subscribe_events(&self) -> broadcast::Receiver<Arc<serde_json::Value>> {
    self.event_tx.subscribe()
  }

  pub fn subscribe_event_method(&self, method: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    match self.method_event_txs.entry(method) {
      Entry::Occupied(entry) => entry.get().subscribe(),
      Entry::Vacant(entry) => {
        let (tx, rx) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        entry.insert(tx);
        rx
      },
    }
  }

  pub fn subscribe_event_domain(&self, domain: &'static str) -> broadcast::Receiver<Arc<serde_json::Value>> {
    match self.domain_event_txs.entry(domain) {
      Entry::Occupied(entry) => entry.get().subscribe(),
      Entry::Vacant(entry) => {
        let (tx, rx) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        entry.insert(tx);
        rx
      },
    }
  }

  /// Drain every in-flight `send_command` oneshot and deliver a
  /// `target_closed` error. Called by the reader task on EOF / error
  /// so callers don't block on responses that will never arrive.
  pub fn fail_all_pending(&self, reason: &str) {
    // `DashMap::iter_mut` would hold shard locks; collect keys first.
    let keys: Vec<u64> = self.pending.iter().map(|e| *e.key()).collect();
    for id in keys {
      if let Some((_, entry)) = self.pending.remove(&id) {
        let _ = entry.tx.send(Err(FerriError::target_closed(Some(reason.to_string()))));
      }
    }
  }

  /// Build a CDP command as NUL-terminated JSON bytes and register a response receiver.
  pub fn build_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<(Vec<u8>, oneshot::Receiver<CdpResult>)> {
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

    let (tx, rx) = oneshot::channel();
    let stats_enabled = rtt_stats_enabled();
    let entry = PendingEntry {
      tx,
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
    Ok((data, rx))
  }

  /// Dispatch a raw CDP message (response or event). Called by the reader task.
  pub fn dispatch_message(&self, raw: &[u8]) {
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
        Err(FerriError::protocol("CDP", msg_str))
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
      // Event
      let method = json_scan::json_string(json_scan::json_field(raw, b"method"));
      let session_id = json_scan::json_string(json_scan::json_field(raw, b"sessionId"));
      let method_str = std::str::from_utf8(method).unwrap_or("");
      let sid_str = std::str::from_utf8(session_id).unwrap_or("");

      self.dispatch_lifecycle(raw, method_str, sid_str);

      tracing::trace!(
        target: "ferridriver::cdp::recv",
        method = method_str,
        "CDP << event",
      );

      let method_tx = self.method_event_txs.get(method_str).map(|entry| entry.clone());
      let domain_tx = method_str
        .split_once('.')
        .and_then(|(domain, _)| self.domain_event_txs.get(domain).map(|entry| entry.clone()));
      let needs_global = self.event_tx.receiver_count() > 0;
      let needs_method = method_tx.as_ref().is_some_and(|tx| tx.receiver_count() > 0);
      let needs_domain = domain_tx.as_ref().is_some_and(|tx| tx.receiver_count() > 0);

      if (needs_global || needs_method || needs_domain)
        && let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw)
      {
        let msg = Arc::new(msg);
        if needs_global {
          let _ = self.event_tx.send(msg.clone());
        }
        if needs_method && let Some(tx) = method_tx {
          let _ = tx.send(msg.clone());
        }
        if needs_domain && let Some(tx) = domain_tx {
          let _ = tx.send(msg);
        }
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
          let loader_id_str = std::str::from_utf8(loader_id).unwrap_or("");
          let mut state = lock_or_recover(&tracker.state);
          state.current_loader_id = loader_id_str.to_string();
          state.fired = super::LC_COMMIT;
          drop(state);
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
            let mut state = lock_or_recover(&tracker.state);
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
            if state.current_loader_id == loader_id_str {
              state.fired |= event_flag;
              drop(state);
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
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Instant;

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
}
