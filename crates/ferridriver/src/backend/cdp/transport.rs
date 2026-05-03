//! Transport trait and shared CDP message dispatch logic.
//!
//! The dispatch logic (response correlation, nav waiters, lifecycle tracking,
//! event broadcast) is identical for pipe and WebSocket transports. It lives
//! here as `CdpDispatcher` — both transports embed it and call `dispatch_message`.

use dashmap::DashMap;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{broadcast, oneshot};

use crate::backend::json_scan;

/// Truncate a string for logging, appending "..." if truncated.
fn truncate_for_log(s: &str, max: usize) -> String {
  if s.len() <= max {
    s.to_string()
  } else {
    format!("{}...", &s[..max])
  }
}

/// Result of a single CDP command: either the response value or an error string.
type CdpResult = Result<serde_json::Value, String>;

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
  send_at: Instant,
}

/// Pending-command map: command ID -> [`PendingEntry`].
///
/// Sharded via [`dashmap::DashMap`] so concurrent senders don't
/// serialise on a single global mutex. Each shard has its own
/// internal RwLock; insert/remove on different keys are wait-free
/// vs each other. Replaced `Arc<std::sync::Mutex<FxHashMap>>` —
/// uncontended insert went from ~100ns (mutex acq + HashMap insert)
/// to ~50ns (shard lookup + per-shard insert), and contention at
/// 4+ concurrent senders no longer serialises. The per-key mutex
/// model would be even cheaper but requires `parking_lot::Mutex`
/// per entry which complicates the lifetime story for one-shot
/// senders. DashMap is the right balance.
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

  fn dump(&self) {
    if self.buckets.is_empty() {
      return;
    }
    let mut rows: Vec<(&String, &RttBucket)> = self.buckets.iter().collect();
    rows.sort_by(|a, b| b.1.total_ns.cmp(&a.1.total_ns));
    let total_count: u64 = self.buckets.values().map(|b| b.count).sum();
    let total_ns: u128 = self.buckets.values().map(|b| b.total_ns).sum();
    eprintln!(
      "─── ferridriver CDP RTT stats ─── total_calls={total_count}  total_time={:.1}ms",
      total_ns as f64 / 1_000_000.0
    );
    eprintln!(
      "  {:<48}  {:>7}  {:>10}  {:>10}  {:>10}",
      "method", "count", "total_ms", "avg_us", "max_us"
    );
    for (method, bucket) in rows {
      let avg_us = bucket.total_ns as f64 / bucket.count as f64 / 1000.0;
      eprintln!(
        "  {:<48}  {:>7}  {:>10.2}  {:>10.1}  {:>10.1}",
        method,
        bucket.count,
        bucket.total_ns as f64 / 1_000_000.0,
        avg_us,
        bucket.max_ns as f64 / 1000.0,
      );
    }
  }
}

/// Returns true when the `FERRIDRIVER_RTT_STATS` env var is set to
/// any truthy value (`1`, `true`, `yes`). Cached via [`std::sync::OnceLock`]
/// so the env-var lookup happens once per process.
fn rtt_stats_enabled() -> bool {
  static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  *ENABLED.get_or_init(|| {
    std::env::var("FERRIDRIVER_RTT_STATS")
      .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
      .unwrap_or(false)
  })
}

/// Trait abstracting CDP transport medium (pipes vs WebSocket).
pub trait CdpTransport: Send + Sync + 'static {
  fn send_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: serde_json::Value,
  ) -> impl std::future::Future<Output = Result<serde_json::Value, String>> + Send;

  fn register_nav_waiter(
    &self,
    session_id: &str,
    target: crate::backend::NavLifecycle,
  ) -> oneshot::Receiver<Result<(), String>>;

  fn subscribe_events(&self) -> broadcast::Receiver<Arc<serde_json::Value>>;

  fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
  );
}

// ── Shared dispatch state ──────────────────────────────────────────────────

struct NavWaiter {
  target: crate::backend::NavLifecycle,
  tx: oneshot::Sender<Result<(), String>>,
}

pub(crate) struct LifecycleTracker {
  pub state: Arc<std::sync::Mutex<super::LifecycleState>>,
  pub notify: Arc<tokio::sync::Notify>,
}

/// Shared CDP message dispatch state. Embedded by both `PipeTransport` and `WsTransport`.
pub(crate) struct CdpDispatcher {
  pub next_id: AtomicU64,
  pub pending: Arc<PendingMap>,
  /// Per-session navigation waiters (keyed by sessionId). Sharded
  /// via DashMap so events firing on N sessions don't contend on
  /// the same mutex.
  nav_waiters: Arc<DashMap<String, NavWaiter>>,
  /// Per-session lifecycle trackers (keyed by sessionId). Sharded
  /// via DashMap; same reasoning as `nav_waiters`.
  lifecycle_trackers: Arc<DashMap<String, LifecycleTracker>>,
  /// Per-message broadcast channel. Wraps the message in `Arc` so
  /// fanout to N subscribers is N refcount bumps (~5ns each)
  /// instead of N deep `serde_json::Value` clones (~400ns + ~10
  /// allocs each). At 200 events/s × ~12 subscribers per page this
  /// is the single biggest hot-loop CPU win in transport.
  pub event_tx: broadcast::Sender<Arc<serde_json::Value>>,
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

impl CdpDispatcher {
  pub fn new() -> Self {
    let (event_tx, _) = broadcast::channel(256);
    Self {
      next_id: AtomicU64::new(1),
      pending: Arc::new(DashMap::default()),
      nav_waiters: Arc::new(DashMap::default()),
      lifecycle_trackers: Arc::new(DashMap::default()),
      event_tx,
      rtt_stats: Arc::new(std::sync::Mutex::new(RttStats::default())),
    }
  }

  pub fn register_nav_waiter(
    &self,
    session_id: &str,
    target: crate::backend::NavLifecycle,
  ) -> oneshot::Receiver<Result<(), String>> {
    let (tx, rx) = oneshot::channel();
    self
      .nav_waiters
      .insert(session_id.to_string(), NavWaiter { target, tx });
    rx
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

  /// Build a CDP command as NUL-terminated JSON bytes and register a response receiver.
  pub fn build_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<(Vec<u8>, oneshot::Receiver<CdpResult>), String> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let params_str = serde_json::to_string(params).map_err(|e| format!("Serialize: {e}"))?;
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
    let entry = PendingEntry {
      tx,
      // Allocate the method String only when stats are enabled —
      // saves the per-command alloc when stats are off (the common
      // path).
      method: if rtt_stats_enabled() {
        method.to_string()
      } else {
        String::new()
      },
      send_at: Instant::now(),
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
        Err(msg_str.to_string())
      };
      tracing::debug!(
        target: "ferridriver::cdp::recv",
        id,
        ok = payload.is_ok(),
        payload = truncate_for_log(&format!("{payload:?}"), 200),
        "CDP << response",
      );
      if let Some((_, entry)) = self.pending.remove(&id) {
        if rtt_stats_enabled() {
          let elapsed = entry.send_at.elapsed().as_nanos();
          lock_or_recover(&self.rtt_stats).record(&entry.method, elapsed);
        }
        let _ = entry.tx.send(payload);
      }
    } else {
      // Event
      let method = json_scan::json_string(json_scan::json_field(raw, b"method"));
      let session_id = json_scan::json_string(json_scan::json_field(raw, b"sessionId"));
      let method_str = std::str::from_utf8(method).unwrap_or("");
      let sid_str = std::str::from_utf8(session_id).unwrap_or("");
      let key = sid_str.to_string();

      // Nav waiter dispatch. DashMap pattern: lookup target via
      // `get` (returns a guard that holds the shard read-lock for
      // the lookup window only), then `remove` if matched. We don't
      // hold the get-guard across the remove — pattern below clones
      // the matched target via `Copy` so the read guard drops
      // immediately, then `remove` takes the write lock cleanly.
      {
        use crate::backend::NavLifecycle;
        let target_now = self.nav_waiters.get(&key).map(|g| g.target);
        let take_if = |want: bool| -> Option<NavWaiter> {
          if want {
            self.nav_waiters.remove(&key).map(|(_, v)| v)
          } else {
            None
          }
        };
        match method_str {
          "Page.frameNavigated" => {
            if let Some(w) = take_if(matches!(target_now, Some(NavLifecycle::Commit))) {
              let _ = w.tx.send(Ok(()));
            }
          },
          "Page.lifecycleEvent" => {
            let params = json_scan::json_field(raw, b"params");
            let name = json_scan::json_string(json_scan::json_field(params, b"name"));
            let name_str = std::str::from_utf8(name).unwrap_or("");
            let resolve = matches!(
              (name_str, target_now),
              ("DOMContentLoaded", Some(NavLifecycle::DomContentLoaded))
                | ("load", Some(NavLifecycle::Load | NavLifecycle::DomContentLoaded))
            );
            if let Some(w) = take_if(resolve) {
              let _ = w.tx.send(Ok(()));
            }
          },
          "Page.loadEventFired" => {
            let resolve = matches!(target_now, Some(NavLifecycle::Load | NavLifecycle::DomContentLoaded));
            if let Some(w) = take_if(resolve) {
              let _ = w.tx.send(Ok(()));
            }
          },
          "Page.domContentEventFired" => {
            let resolve = matches!(target_now, Some(NavLifecycle::DomContentLoaded));
            if let Some(w) = take_if(resolve) {
              let _ = w.tx.send(Ok(()));
            }
          },
          "Inspector.targetCrashed" => {
            if let Some((_, w)) = self.nav_waiters.remove(&key) {
              let _ = w.tx.send(Err("Target crashed".into()));
            }
          },
          _ => {},
        }
      }

      self.dispatch_lifecycle(raw, method_str, &key);

      tracing::trace!(
        target: "ferridriver::cdp::recv",
        method = method_str,
        "CDP << event",
      );

      // Broadcast (full parse for console/network listeners). Wrap
      // the parsed Value in `Arc` so fan-out to N subscribers is N
      // refcount bumps instead of N deep clones — see field doc on
      // `event_tx` above.
      //
      // TODO: skip the parse entirely when `event_tx.receiver_count()
      // == 0` to save the per-event 600ns + 10-alloc cost on
      // workloads that have no event subscribers (rare in tests but
      // common in raw-action MCP usage).
      if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw) {
        let _ = self.event_tx.send(Arc::new(msg));
      }
    }
  }

  /// Lifecycle tracker dispatch -- tracks `loaderId` for document-accurate lifecycle.
  fn dispatch_lifecycle(&self, raw: &[u8], method_str: &str, key: &str) {
    if let Some(tracker) = self.lifecycle_trackers.get(key) {
      match method_str {
        "Page.frameNavigated" => {
          let params = json_scan::json_field(raw, b"params");
          let frame = json_scan::json_field(params, b"frame");
          let loader_id = json_scan::json_string(json_scan::json_field(frame, b"loaderId"));
          let loader_id_str = std::str::from_utf8(loader_id).unwrap_or("");
          let mut state = lock_or_recover(&tracker.state);
          state.current_loader_id = loader_id_str.to_string();
          state.fired.clear();
          state.fired.insert("commit".to_string());
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
            "DOMContentLoaded" => Some("domcontentloaded"),
            "load" => Some("load"),
            _ => None,
          };
          if let Some(event_name) = event_name {
            let mut state = lock_or_recover(&tracker.state);
            if state.current_loader_id == loader_id_str || state.current_loader_id.is_empty() {
              state.fired.insert(event_name.to_string());
              drop(state);
              tracker.notify.notify_waiters();
            }
          }
        },
        _ => {},
      }
    }
  }
}

impl Drop for CdpDispatcher {
  fn drop(&mut self) {
    // Dump per-method RTT stats on transport teardown when stats
    // collection is enabled. Catches both clean shutdowns and
    // process-exit drops via the transport's `Arc` chain. When stats
    // collection is off the bucket map is empty and `dump` is a
    // no-op.
    if rtt_stats_enabled() {
      lock_or_recover(&self.rtt_stats).dump();
    }
  }
}
