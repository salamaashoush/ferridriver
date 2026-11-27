//! Shared browser state and per-context lock-free caches for [`crate::server::McpServer`].

use arc_swap::ArcSwap;
use dashmap::DashMap;
use ferridriver::state::{BrowserState, ContextLogHandles};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Shared state for the MCP server.
///
/// Hot paths (`ref_map` reads, log reads) use extracted `Arc` handles cached in
/// `DashMap`s and bypass the `RwLock` entirely. Cold paths (instance init, page
/// management) use the `RwLock<BrowserState>`.
#[derive(Clone)]
pub struct SharedState {
  /// The underlying browser state. Write-locked only for mutations
  /// (`ensure_instance`, `open_page`, `close_page`, `shutdown`, `connect`).
  /// Read-locked for lookups that extract `Arc` handles.
  inner: Arc<RwLock<BrowserState>>,
  /// Cached `ref_map` handles per context — wait-free reads via `ArcSwap`.
  ref_maps: Arc<DashMap<String, RefMapHandle>>,
  /// Cached log handles per context.
  log_handles: Arc<DashMap<String, ContextLogHandles>>,
  /// Per-context serialization locks (replaces nested `Mutex<HashMap<..>>`).
  pub(super) context_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

/// Type alias for the `ArcSwap`-wrapped ref map used for wait-free reads.
type RefMapHandle = Arc<ArcSwap<FxHashMap<String, i64>>>;

impl SharedState {
  pub(super) fn new(browser_state: BrowserState) -> Self {
    Self {
      inner: browser_state.into_shared(),
      ref_maps: Arc::new(DashMap::new()),
      log_handles: Arc::new(DashMap::new()),
      context_locks: Arc::new(DashMap::new()),
    }
  }

  /// Write-lock the inner state (for mutations).
  pub(crate) async fn write(&self) -> tokio::sync::RwLockWriteGuard<'_, BrowserState> {
    self.inner.write().await
  }

  /// Read-lock the inner state (for lookups).
  pub(crate) async fn read(&self) -> tokio::sync::RwLockReadGuard<'_, BrowserState> {
    self.inner.read().await
  }

  /// Current generation of the browser instance backing `context`'s
  /// session, or `None` if no such instance is live. Used to detect a
  /// browser-session swap (relaunch/reconnect) so a stale script VM is
  /// discarded rather than left holding handles into a dead session.
  pub(crate) async fn instance_generation(&self, context: &str) -> Option<u64> {
    let state = self.inner.read().await;
    let key = state.session_key(context);
    state.instance_generation(key.instance.as_ref())
  }

  /// Parse a session key against the state's configured instance names.
  pub(crate) async fn session_key(&self, raw: &str) -> ferridriver::state::SessionKey {
    self.inner.read().await.session_key(raw)
  }

  /// The configured instance names, for a caller that must parse several
  /// keys without retaking the lock each time.
  pub(crate) async fn known_instances(&self) -> ferridriver::state::KnownInstances {
    self.inner.read().await.known_instances()
  }

  /// Get a cached `ArcSwap` handle for storing `ref_map`s (wait-free store).
  pub(crate) async fn ref_map_handle(&self, context: &str) -> Option<RefMapHandle> {
    if let Some(entry) = self.ref_maps.get(context) {
      return Some(Arc::clone(entry.value()));
    }
    let state = self.inner.read().await;
    let handle = state.ref_map_handle(context)?;
    drop(state);
    self.ref_maps.insert(context.to_string(), Arc::clone(&handle));
    Some(handle)
  }

  /// Get cached log handles for a context (no `BrowserState` lock after first call).
  pub(crate) async fn log_handles_for(&self, context: &str) -> Option<ContextLogHandles> {
    if let Some(entry) = self.log_handles.get(context) {
      return Some(entry.value().clone());
    }
    let state = self.inner.read().await;
    let handles = state.log_handles(context)?;
    drop(state);
    self.log_handles.insert(context.to_string(), handles.clone());
    Some(handles)
  }

  /// Invalidate caches for a context (after `close_page`, new page, etc.).
  ///
  /// The per-context serialization lock deliberately survives: dropping
  /// it while a tool call holds the guard means the next caller mints a
  /// fresh mutex and runs concurrently with the guard holder, which is
  /// exactly what the lock exists to prevent. Cold-starting a context
  /// invalidates from inside such a guard, so this was reachable. The
  /// entries are one small `Arc` per distinct session name.
  pub(crate) fn invalidate_context(&self, context: &str) {
    self.ref_maps.remove(context);
    self.log_handles.remove(context);
  }

  /// Invalidate all caches (after shutdown).
  pub(crate) fn invalidate_all(&self) {
    self.ref_maps.clear();
    self.log_handles.clear();
  }

  /// Get a clone of the inner `Arc<RwLock<BrowserState>>` for constructing `ContextRef`.
  pub(crate) fn state_arc(&self) -> Arc<RwLock<BrowserState>> {
    Arc::clone(&self.inner)
  }
}

/// Backward-compat type alias.
pub type State = SharedState;

/// Backward-compat free function: derive context from session only.
#[must_use]
pub fn ctx(s: Option<&String>) -> &str {
  s.map_or("default", String::as_str)
}

// Backward-compat alias so existing tool code keeps compiling during transition.
pub use self::ctx as sess;
