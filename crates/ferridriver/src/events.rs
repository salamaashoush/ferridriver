//! Event system for Page, Frame, and `BrowserContext`.
//!
//! Playwright-compatible event emitter using tokio broadcast channels.
//! Supports `on()`, `once()`, `waitForEvent()` patterns.
//!
//! Events flow from backend (CDP/WebKit) -> `EventEmitter` -> subscribers.
//! The existing log accumulation (`console_log`, `network_log`, `dialog_log`)
//! continues working alongside the new event system.

use crate::backend::FrameInfo;
use crate::context::{ConsoleMsg, NetRequest};
use std::sync::Arc;
use tokio::sync::broadcast;

// ── Event Types ──────────────────────────────────────────────────────────────

/// Events emitted by pages. Mirrors Playwright's page event types.
#[derive(Debug, Clone)]
pub enum PageEvent {
  /// Console message from the page (console.log, console.error, etc.)
  Console(ConsoleMsg),
  /// Network request started.
  Request(NetRequest),
  /// Network response received.
  Response(NetResponse),
  /// Dialog appeared (alert, confirm, prompt).
  Dialog(PendingDialog),
  /// Frame attached to the page.
  FrameAttached(FrameInfo),
  /// Frame detached from the page.
  FrameDetached { frame_id: String },
  /// Frame navigated to a new URL.
  FrameNavigated(FrameInfo),
  /// Page load event fired.
  Load,
  /// `DOMContentLoaded` event fired.
  DomContentLoaded,
  /// Page was closed.
  Close,
  /// Uncaught exception or unhandled rejection in page.
  PageError(String),
  /// Download started.
  Download(DownloadInfo),
}

/// Information about a download.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DownloadInfo {
  /// Unique identifier for this download.
  pub guid: String,
  /// URL that triggered the download.
  pub url: String,
  /// Suggested filename from the server.
  pub suggested_filename: String,
}

/// Network response data with headers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NetResponse {
  pub request_id: String,
  pub url: String,
  pub status: i64,
  pub status_text: String,
  pub mime_type: String,
  /// Response headers (key -> value).
  #[serde(skip_serializing_if = "Option::is_none")]
  pub headers: Option<rustc_hash::FxHashMap<String, String>>,
}

/// A dialog that is pending user action (accept/dismiss).
#[derive(Debug, Clone)]
pub struct PendingDialog {
  /// Dialog type: "alert", "confirm", "prompt", "beforeunload"
  pub dialog_type: String,
  /// The message displayed in the dialog.
  pub message: String,
  /// Default value for prompt dialogs.
  pub default_value: String,
}

/// How to respond to a dialog.
#[derive(Debug, Clone)]
pub enum DialogAction {
  /// Accept the dialog, optionally with prompt text.
  Accept(Option<String>),
  /// Dismiss (cancel) the dialog.
  Dismiss,
}

/// Dialog handler function type.
/// Takes dialog info, returns the action to take.
/// Must be Send + Sync since it's called from async tasks.
pub type DialogHandler = Arc<dyn Fn(&PendingDialog) -> DialogAction + Send + Sync>;

/// Callback type for exposed functions.
/// Takes serialized JSON arguments, returns serialized JSON result.
pub type ExposedFn = Arc<dyn Fn(Vec<serde_json::Value>) -> serde_json::Value + Send + Sync>;

/// Event listener callback type.
pub type EventCallback = Arc<dyn Fn(PageEvent) + Send + Sync>;

/// Handle for removing an event listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(pub u64);

/// Default dialog handler: accept alerts/confirms, dismiss prompts.
#[must_use]
pub fn default_dialog_handler() -> DialogHandler {
  Arc::new(|dialog| {
    if dialog.dialog_type == "prompt" {
      DialogAction::Accept(Some(dialog.default_value.clone()))
    } else {
      DialogAction::Accept(None)
    }
  })
}

/// Check if an event matches a named event type.
fn event_name_matches(name: &str, event: &PageEvent) -> bool {
  matches!(
    (name, event),
    ("console", PageEvent::Console(_))
      | ("request", PageEvent::Request(_))
      | ("response", PageEvent::Response(_))
      | ("dialog", PageEvent::Dialog(_))
      | ("frameattached", PageEvent::FrameAttached(_))
      | ("framedetached", PageEvent::FrameDetached { .. })
      | ("framenavigated", PageEvent::FrameNavigated(_))
      | ("load", PageEvent::Load)
      | ("domcontentloaded", PageEvent::DomContentLoaded)
      | ("close", PageEvent::Close)
      | ("pageerror", PageEvent::PageError(_))
      | ("download", PageEvent::Download(_))
  )
}

// ── Event Emitter ────────────────────────────────────────────────────────────

/// Broadcast-based event emitter. Cheap to clone (Arc'd internally).
#[derive(Clone)]
pub struct EventEmitter {
  tx: broadcast::Sender<PageEvent>,
  /// Active listeners with their abort handles.
  listeners: Arc<std::sync::Mutex<rustc_hash::FxHashMap<u64, tokio::task::AbortHandle>>>,
  next_listener_id: Arc<std::sync::atomic::AtomicU64>,
  /// Stored runtime handle so `on()` works from non-async contexts (NAPI).
  runtime_handle: Arc<std::sync::Mutex<Option<tokio::runtime::Handle>>>,
}

impl EventEmitter {
  #[must_use]
  pub fn new() -> Self {
    let (tx, _) = broadcast::channel(512);
    // Try to capture the current runtime handle at creation time.
    let handle = tokio::runtime::Handle::try_current().ok();
    Self {
      tx,
      listeners: Arc::new(std::sync::Mutex::new(rustc_hash::FxHashMap::default())),
      next_listener_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
      runtime_handle: Arc::new(std::sync::Mutex::new(handle)),
    }
  }

  /// Spawn a task on the captured runtime (works from non-async contexts).
  fn spawn_listener(&self, future: impl std::future::Future<Output = ()> + Send + 'static) -> tokio::task::AbortHandle {
    // Try current runtime first, then fall back to stored handle
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
      return handle.spawn(future).abort_handle();
    }
    if let Ok(guard) = self.runtime_handle.lock() {
      if let Some(handle) = guard.as_ref() {
        return handle.spawn(future).abort_handle();
      }
    }
    // Last resort: spawn a thread with its own runtime
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
      let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
      };
      let handle = rt.spawn(future);
      let _ = tx.send(handle.abort_handle());
      rt.block_on(handle).ok();
    });
    rx.recv().unwrap_or_else(|_| {
      // Fallback: return a no-op abort handle
      tokio::runtime::Handle::current().spawn(async {}).abort_handle()
    })
  }

  /// Emit an event to all current subscribers.
  pub fn emit(&self, event: PageEvent) {
    let _ = self.tx.send(event);
  }

  /// Subscribe to events. Returns a receiver that gets all future events.
  #[must_use]
  pub fn subscribe(&self) -> broadcast::Receiver<PageEvent> {
    self.tx.subscribe()
  }

  /// Wait for an event matching a predicate, with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the event channel is closed.
  ///
  /// ```ignore
  /// let response = emitter.wait_for(|e| matches!(e, PageEvent::Response(r) if r.url.contains("/api")), 5000).await?;
  /// ```
  pub async fn wait_for<F>(&self, predicate: F, timeout_ms: u64) -> Result<PageEvent, String>
  where
    F: Fn(&PageEvent) -> bool,
  {
    let mut rx = self.tx.subscribe();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

    loop {
      let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
      if remaining.is_zero() {
        return Err("Timeout waiting for event".into());
      }
      match tokio::time::timeout(remaining, rx.recv()).await {
        Ok(Ok(event)) if predicate(&event) => return Ok(event),
        Ok(Ok(_)) => {},
        Ok(Err(_)) => return Err("Event channel closed".into()),
        Err(_) => return Err("Timeout waiting for event".into()),
      }
    }
  }

  /// Wait for the next event of a specific type, with timeout.
  ///
  /// # Errors
  ///
  /// Returns an error if the timeout elapses or the event channel is closed.
  pub async fn wait_for_event(&self, event_name: &str, timeout_ms: u64) -> Result<PageEvent, String> {
    let name = event_name.to_string();
    self.wait_for(move |e| event_name_matches(&name, e), timeout_ms).await
  }

  /// Subscribe to events matching a name. Calls the callback for each matching event.
  /// Returns a `ListenerId` for later removal with `off()`.
  ///
  /// # Panics
  ///
  /// Panics if the internal listeners mutex is poisoned.
  pub fn on(&self, event_name: &str, callback: EventCallback) -> ListenerId {
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let name = event_name.to_string();

    let abort_handle = self.spawn_listener(async move {
      while let Ok(event) = rx.recv().await {
        if event_name_matches(&name, &event) {
          callback(event);
        }
      }
    });

    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(id, abort_handle);
    }
    ListenerId(id)
  }

  /// Subscribe to a single event, then auto-remove the listener.
  ///
  /// Subscribe to a single event, then auto-remove the listener.
  pub fn once(&self, event_name: &str, callback: EventCallback) -> ListenerId {
    let listeners = self.listeners.clone();
    let id = self.next_listener_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut rx = self.tx.subscribe();
    let name = event_name.to_string();

    let abort_handle = self.spawn_listener(async move {
      while let Ok(event) = rx.recv().await {
        if event_name_matches(&name, &event) {
          callback(event);
          if let Ok(mut guard) = listeners.lock() {
            guard.remove(&id);
          }
          break;
        }
      }
    });

    if let Ok(mut guard) = self.listeners.lock() {
      guard.insert(id, abort_handle);
    }
    ListenerId(id)
  }

  /// Remove an event listener by ID.
  ///
  /// Remove an event listener by ID.
  pub fn off(&self, id: ListenerId) {
    if let Ok(mut guard) = self.listeners.lock() {
      if let Some(handle) = guard.remove(&id.0) {
        handle.abort();
      }
    }
  }

  /// Remove all event listeners.
  ///
  /// Remove all event listeners.
  pub fn remove_all_listeners(&self) {
    if let Ok(mut listeners) = self.listeners.lock() {
      for (_, handle) in listeners.drain() {
        handle.abort();
      }
    }
  }
}

impl Default for EventEmitter {
  fn default() -> Self {
    Self::new()
  }
}
