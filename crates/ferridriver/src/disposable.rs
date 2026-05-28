//! Playwright parity: `client/disposable.ts` `Disposable` / `DisposableStub`.
//!
//! Playwright's `route`, `addInitScript`, `exposeBinding`, and
//! `exposeFunction` return a handle with an async `dispose()` method that
//! reverses the registration (unroute / remove-init-script / unbind). The
//! `DisposableStub` form wraps a `() => Promise<void>` closure that runs once
//! and is idempotent afterwards. This is that single canonical type, owned by
//! core; the `NAPI` and `QuickJS` layers wrap it as a JS object exposing
//! `dispose()` (and the `remove()` alias).

use crate::error::Result;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

/// Boxed one-shot async closure that reverses a registration.
type DisposeFn = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<()>> + Send>> + Send>;

/// Reverses a single registration (route, init-script, exposed binding).
///
/// Mirrors Playwright's `DisposableStub`: the first `dispose()` runs the
/// underlying unbind closure; subsequent calls are no-ops (the closure is
/// consumed). `dispose()` and its `remove()` alias are interchangeable.
pub struct Disposable {
  dispose_fn: Mutex<Option<DisposeFn>>,
}

impl std::fmt::Debug for Disposable {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let consumed = self.dispose_fn.lock().map_or(true, |g| g.is_none());
    f.debug_struct("Disposable").field("consumed", &consumed).finish()
  }
}

impl Disposable {
  /// Construct from a one-shot async unbind closure.
  pub fn new<F, Fut>(dispose: F) -> Self
  where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
  {
    Self {
      dispose_fn: Mutex::new(Some(Box::new(move || Box::pin(dispose())))),
    }
  }

  /// Run the unbind closure exactly once. Repeat calls are no-ops and return
  /// `Ok(())`, matching Playwright's `DisposableStub.dispose` semantics.
  ///
  /// # Errors
  ///
  /// Propagates any error returned by the underlying unbind closure.
  pub async fn dispose(&self) -> Result<()> {
    let taken = {
      let mut guard = self
        .dispose_fn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      guard.take()
    };
    match taken {
      Some(f) => f().await,
      None => Ok(()),
    }
  }

  /// Alias for [`Disposable::dispose`]. Playwright's TS surface exposes the
  /// reverse-registration handle through `dispose()`; ferridriver also offers
  /// `remove()` because the historical ferridriver API named these
  /// `unroute` / `removeInitScript`.
  ///
  /// # Errors
  ///
  /// Propagates any error returned by the underlying unbind closure.
  pub async fn remove(&self) -> Result<()> {
    self.dispose().await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[tokio::test]
  async fn dispose_runs_once_then_is_noop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_closure = Arc::clone(&calls);
    let d = Disposable::new(move || {
      let calls = Arc::clone(&calls_for_closure);
      async move {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
      }
    });

    d.dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Repeat dispose() is a no-op.
    d.dispose().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // remove() is an alias and also a no-op after consumption.
    d.remove().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn remove_is_alias_for_dispose() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_closure = Arc::clone(&calls);
    let d = Disposable::new(move || {
      let calls = Arc::clone(&calls_for_closure);
      async move {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
      }
    });

    d.remove().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
  }

  #[tokio::test]
  async fn dispose_propagates_error() {
    let d = Disposable::new(|| async { Err(crate::error::FerriError::invalid_argument("x", "boom")) });
    assert!(d.dispose().await.is_err());
  }
}
