//! Task-local partial-resource retention for cancellable browser startup.
//!
//! During browser startup (launch or connect), multiple resources may be
//! allocated sequentially (e.g., an Android/iOS device tunnel, a driver
//! process, a `WebDriver` session, or a `WebKit` owner). Registering each
//! resource in the current allocation scope ensures that if startup fails or
//! is cancelled, every partially allocated resource is cleaned up in reverse
//! order — and retained for retry if cleanup itself fails.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use ferridriver_error::Result;

/// A startup resource that can report whether cleanup finished and perform
/// idempotent async cleanup on startup failure or allocation shutdown.
pub trait AllocationResource: Send + Sync {
  /// Returns `true` once cleanup has completed and no further close attempt
  /// is needed.
  fn cleanup_complete(&self) -> bool;

  /// Perform cleanup for this resource.
  fn close(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

  /// Whether this resource represents a live remote session (e.g. a
  /// `WebDriver` session) that must finish deleting before its backing driver
  /// or device tunnel shuts down.
  fn is_session(&self) -> bool {
    false
  }
}

/// Shared list of resources retained by an in-flight allocation scope.
#[derive(Default)]
pub struct ResourceList {
  items: Mutex<Vec<Arc<dyn AllocationResource>>>,
}

impl ResourceList {
  /// Create an empty resource list.
  #[must_use]
  pub fn new() -> Arc<Self> {
    Arc::new(Self::default())
  }

  /// Record a newly allocated resource for reverse-order cleanup.
  pub fn push(&self, resource: Arc<dyn AllocationResource>) {
    self
      .items
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(resource);
  }

  /// Returns `true` when no partial resources are currently retained.
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self
      .items
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_empty()
  }

  /// Drop all retained references (called once startup succeeds or cleanup
  /// finishes).
  pub fn clear(&self) {
    self
      .items
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clear();
  }

  /// Keep only resources whose cleanup has not completed yet.
  pub fn retain_incomplete(&self) {
    self
      .items
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .retain(|resource| !resource.cleanup_complete());
  }

  /// Clone the current retained resource handles for async cleanup without
  /// holding the mutex across `.await`.
  #[must_use]
  pub fn snapshot(&self) -> Vec<Arc<dyn AllocationResource>> {
    self
      .items
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  /// Drain all retained resource handles (used from `Drop`).
  #[must_use]
  pub fn take_all(&self) -> Vec<Arc<dyn AllocationResource>> {
    std::mem::take(&mut *self.items.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
  }

  /// Returns `true` if any retained session resource has not finished
  /// deleting yet.
  #[must_use]
  pub fn has_pending_sessions(&self) -> bool {
    self
      .items
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .any(|resource| resource.is_session() && !resource.cleanup_complete())
  }
}

tokio::task_local! {
  static CURRENT: Arc<ResourceList>;
}

/// Run `fut` with `list` installed as the active allocation resource scope.
pub async fn scope<F: Future>(list: Arc<ResourceList>, fut: F) -> F::Output {
  CURRENT.scope(list, fut).await
}

/// Retain `resource` in the active allocation scope, if any.
pub fn retain<R: AllocationResource + 'static>(resource: &Arc<R>) {
  let cloned: Arc<R> = Arc::clone(resource);
  let resource: Arc<dyn AllocationResource> = cloned;
  let _ = CURRENT.try_with(|list| list.push(resource));
}

/// Returns `true` if the active allocation scope holds a live session whose
/// deletion has not completed yet.
#[must_use]
pub fn has_pending_sessions() -> bool {
  CURRENT.try_with(|list| list.has_pending_sessions()).unwrap_or(false)
}
