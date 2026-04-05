//! Transport trait and shared CDP message dispatch logic.
//!
//! The dispatch logic (response correlation, nav waiters, lifecycle tracking,
//! event broadcast) is identical for pipe and WebSocket transports. It lives
//! here as `CdpDispatcher` — both transports embed it and call `dispatch_message`.

use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{broadcast, oneshot};

use crate::backend::json_scan;

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

  fn subscribe_events(&self) -> broadcast::Receiver<serde_json::Value>;

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
  pub pending: Arc<std::sync::Mutex<FxHashMap<u64, oneshot::Sender<Result<serde_json::Value, String>>>>>,
  nav_waiters: Arc<std::sync::Mutex<FxHashMap<String, NavWaiter>>>,
  lifecycle_trackers: Arc<std::sync::Mutex<FxHashMap<String, LifecycleTracker>>>,
  pub event_tx: broadcast::Sender<serde_json::Value>,
}

impl CdpDispatcher {
  pub fn new() -> Self {
    let (event_tx, _) = broadcast::channel(256);
    Self {
      next_id: AtomicU64::new(1),
      pending: Arc::new(std::sync::Mutex::new(FxHashMap::default())),
      nav_waiters: Arc::new(std::sync::Mutex::new(FxHashMap::default())),
      lifecycle_trackers: Arc::new(std::sync::Mutex::new(FxHashMap::default())),
      event_tx,
    }
  }

  pub fn register_nav_waiter(
    &self,
    session_id: &str,
    target: crate::backend::NavLifecycle,
  ) -> oneshot::Receiver<Result<(), String>> {
    let (tx, rx) = oneshot::channel();
    self.nav_waiters.lock().unwrap().insert(session_id.to_string(), NavWaiter { target, tx });
    rx
  }

  pub fn register_lifecycle_tracker(
    &self,
    session_id: &str,
    state: Arc<std::sync::Mutex<super::LifecycleState>>,
    notify: Arc<tokio::sync::Notify>,
  ) {
    self.lifecycle_trackers.lock().unwrap().insert(
      session_id.to_string(),
      LifecycleTracker { state, notify },
    );
  }

  pub fn subscribe_events(&self) -> broadcast::Receiver<serde_json::Value> {
    self.event_tx.subscribe()
  }

  /// Build a CDP command as NUL-terminated JSON bytes and register a response receiver.
  pub fn build_command(
    &self,
    session_id: Option<&str>,
    method: &str,
    params: &serde_json::Value,
  ) -> Result<(Vec<u8>, oneshot::Receiver<Result<serde_json::Value, String>>), String> {
    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
    let params_str = serde_json::to_string(params).map_err(|e| format!("Serialize: {e}"))?;
    let mut data = if let Some(sid) = session_id {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str},"sessionId":"{sid}"}}"#).into_bytes()
    } else {
      format!(r#"{{"id":{id},"method":"{method}","params":{params_str}}}"#).into_bytes()
    };
    data.push(0);

    let (tx, rx) = oneshot::channel();
    self.pending.lock().unwrap().insert(id, tx);
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
          let val: serde_json::Value = serde_json::from_slice(result_field)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
          Ok(val)
        }
      } else {
        let msg_bytes = json_scan::error_message(error_field);
        let msg_str = std::str::from_utf8(msg_bytes).unwrap_or("CDP error");
        Err(msg_str.to_string())
      };
      if let Some(sender) = self.pending.lock().unwrap().remove(&id) {
        let _ = sender.send(payload);
      }
    } else {
      // Event
      let method = json_scan::json_string(json_scan::json_field(raw, b"method"));
      let session_id = json_scan::json_string(json_scan::json_field(raw, b"sessionId"));
      let method_str = std::str::from_utf8(method).unwrap_or("");
      let sid_str = std::str::from_utf8(session_id).unwrap_or("");
      let key = sid_str.to_string();

      // Nav waiter dispatch
      {
        use crate::backend::NavLifecycle;
        let mut waiters = self.nav_waiters.lock().unwrap();
        match method_str {
          "Page.frameNavigated" => {
            if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::Commit)) {
              if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
            }
          }
          "Page.lifecycleEvent" => {
            let params = json_scan::json_field(raw, b"params");
            let name = json_scan::json_string(json_scan::json_field(params, b"name"));
            let name_str = std::str::from_utf8(name).unwrap_or("");
            let resolve = matches!(
              (name_str, waiters.get(&key).map(|w| w.target)),
              ("DOMContentLoaded", Some(NavLifecycle::DomContentLoaded))
              | ("load", Some(NavLifecycle::Load | NavLifecycle::DomContentLoaded))
            );
            if resolve {
              if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
            }
          }
          "Page.loadEventFired" => {
            if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::Load | NavLifecycle::DomContentLoaded)) {
              if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
            }
          }
          "Page.domContentEventFired" => {
            if matches!(waiters.get(&key).map(|w| w.target), Some(NavLifecycle::DomContentLoaded)) {
              if let Some(w) = waiters.remove(&key) { let _ = w.tx.send(Ok(())); }
            }
          }
          "Inspector.targetCrashed" => {
            if let Some(w) = waiters.remove(&key) {
              let _ = w.tx.send(Err("Target crashed".into()));
            }
          }
          _ => {}
        }
      }

      // Lifecycle tracker dispatch — tracks loaderId for document-accurate lifecycle.
      {
        let trackers = self.lifecycle_trackers.lock().unwrap();
        if let Some(tracker) = trackers.get(&key) {
          match method_str {
            "Page.frameNavigated" => {
              // Extract loaderId from the frame payload
              let params = json_scan::json_field(raw, b"params");
              let frame = json_scan::json_field(params, b"frame");
              let loader_id = json_scan::json_string(json_scan::json_field(frame, b"loaderId"));
              let loader_id_str = std::str::from_utf8(loader_id).unwrap_or("");
              let mut state = tracker.state.lock().unwrap();
              state.current_loader_id = loader_id_str.to_string();
              state.fired.clear();
              state.fired.insert("commit".to_string());
              drop(state);
              tracker.notify.notify_waiters();
            }
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
                let mut state = tracker.state.lock().unwrap();
                // Only track events for the current document
                if state.current_loader_id == loader_id_str || state.current_loader_id.is_empty() {
                  state.fired.insert(event_name.to_string());
                  drop(state);
                  tracker.notify.notify_waiters();
                }
              }
            }
            _ => {}
          }
        }
      }

      // Broadcast (full parse for console/network listeners)
      if let Ok(msg) = serde_json::from_slice::<serde_json::Value>(raw) {
        let _ = self.event_tx.send(msg);
      }
    }
  }
}
