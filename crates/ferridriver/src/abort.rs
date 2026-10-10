//! Playwright's `signal` option: an operation started under an
//! [`AbortSignal`] stops when the signal fires and fails with
//! `AbortError`.
//!
//! The signal rides in a task-local for the duration of the call, as the
//! operation budget does, so the binding that received the option only
//! has to [`scope`] the call. The traced page and context wrappers and
//! the locator retry loop [`guard`] the work beneath them; the loop also
//! checks between attempts so its call log says where it stopped, and
//! assertion polling reads [`current`] to fail as a timeout would.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{FerriError, Result};

/// The message of an operation refused because its signal had already
/// fired, Playwright's `AbortError` default.
pub const ABORTED: &str = "The operation was aborted";

/// The Rust side of a JS `AbortSignal`. Clones share one state.
#[derive(Clone, Default)]
pub struct AbortSignal {
  inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
  id: AtomicU64,
  aborted: AtomicBool,
  /// Fired before the call it was given to started.
  refused: AtomicBool,
  reason: Mutex<Option<String>>,
  notify: tokio::sync::Notify,
}

impl std::fmt::Debug for AbortSignal {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AbortSignal")
      .field("id", &self.id())
      .field("aborted", &self.is_aborted())
      .finish()
  }
}

impl AbortSignal {
  #[must_use]
  pub fn new() -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let signal = Self::default();
    signal
      .inner
      .id
      .store(NEXT.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
    signal
  }

  /// A signal that had already fired when the call was made: the call is
  /// refused with "The operation was aborted" rather than the reason.
  #[must_use]
  pub fn refused(reason: impl Into<String>) -> Self {
    let signal = Self::new();
    signal.inner.refused.store(true, Ordering::Release);
    signal.abort(reason);
    signal
  }

  /// Identifies the signal to the binding that made it, which keeps the
  /// JS reason to hand back as the error's `cause`.
  #[must_use]
  pub fn id(&self) -> u64 {
    self.inner.id.load(Ordering::Relaxed)
  }

  /// Fire the signal. `reason` is the message an in-flight operation
  /// fails with; only the first call counts.
  pub fn abort(&self, reason: impl Into<String>) {
    let mut slot = self
      .inner
      .reason
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if self.inner.aborted.swap(true, Ordering::AcqRel) {
      return;
    }
    *slot = Some(reason.into());
    drop(slot);
    self.inner.notify.notify_waiters();
  }

  #[must_use]
  pub fn is_aborted(&self) -> bool {
    self.inner.aborted.load(Ordering::Acquire)
  }

  /// The reason the signal fired with, once it has.
  #[must_use]
  pub fn reason(&self) -> Option<String> {
    self
      .inner
      .reason
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  /// Resolves once the signal fires.
  pub async fn aborted(&self) {
    loop {
      let notified = self.inner.notify.notified();
      if self.is_aborted() {
        return;
      }
      notified.await;
    }
  }

  /// The error the call fails with: refused outright, or stopped in
  /// flight with the reason.
  #[must_use]
  pub fn error(&self) -> FerriError {
    let message = if self.inner.refused.load(Ordering::Acquire) {
      ABORTED.to_string()
    } else {
      self.reason().unwrap_or_else(|| ABORTED.to_string())
    };
    FerriError::Aborted {
      message,
      signal: self.id(),
      call_log: Vec::new(),
    }
  }

  /// Whether the call was refused rather than stopped in flight.
  #[must_use]
  pub fn was_refused(&self) -> bool {
    self.inner.refused.load(Ordering::Acquire)
  }
}

tokio::task_local! {
  static CURRENT: AbortSignal;
}

/// Run `fut` with `signal` as the call's signal, if there is one.
pub fn scope<F: Future>(signal: Option<AbortSignal>, fut: F) -> impl Future<Output = F::Output> {
  match signal {
    Some(signal) => futures::future::Either::Left(CURRENT.scope(signal, fut)),
    None => futures::future::Either::Right(fut),
  }
}

/// The signal of the call this task is running, if it has one.
#[must_use]
pub fn current() -> Option<AbortSignal> {
  CURRENT.try_with(Clone::clone).ok()
}

/// Run `fut`, failing with `AbortError` if the call's signal fires
/// first. A signal that fired before the call refuses it outright.
/// `fut` is polled first on every wake, so work that watches the signal
/// itself reports its own, richer error.
///
/// # Errors
///
/// Returns [`FerriError::Aborted`] when the signal fires, or `fut`'s own
/// error.
pub async fn guard<T>(fut: impl Future<Output = Result<T>>) -> Result<T> {
  let Some(signal) = current() else {
    return fut.await;
  };
  if signal.was_refused() {
    return Err(signal.error());
  }
  tokio::select! {
    biased;
    result = fut => result,
    () = signal.aborted() => Err(signal.error()),
  }
}

/// Sleep `duration`, waking early when the call's signal fires.
pub async fn sleep(duration: Duration) {
  match current() {
    Some(signal) => {
      tokio::select! {
        () = tokio::time::sleep(duration) => {},
        () = signal.aborted() => {},
      }
    },
    None => tokio::time::sleep(duration).await,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn guard_refuses_after_abort_and_interrupts_in_flight() {
    let signal = AbortSignal::new();
    let pending = scope(Some(signal.clone()), guard(std::future::pending::<Result<()>>()));
    let aborter = signal.clone();
    tokio::spawn(async move { aborter.abort("foo bar") });
    let error = pending.await.unwrap_err();
    assert_eq!(error.to_string(), "foo bar");
    assert_eq!(error.name(), "AbortError");

    let refused = AbortSignal::refused("too late");
    assert_eq!(
      scope(Some(refused), guard(async { Ok(()) }))
        .await
        .unwrap_err()
        .to_string(),
      ABORTED
    );
  }

  #[tokio::test]
  async fn an_action_stops_on_the_signal_it_was_given() {
    let signal = AbortSignal::new();
    let action = crate::action::Action::<(), ()>::new(|()| Box::pin(std::future::pending())).signal(signal.clone());
    tokio::spawn(async move { signal.abort("stop") });
    let error = action.await.unwrap_err();
    assert_eq!(error.to_string(), "stop");
    assert_eq!(error.name(), "AbortError");

    let refused = crate::action::Action::<(), u8>::new(|()| Box::pin(async { Ok(1) }))
      .maybe_signal(Some(AbortSignal::refused("too late")));
    assert_eq!(refused.await.unwrap_err().to_string(), ABORTED);
  }

  #[tokio::test]
  async fn no_signal_runs_the_future_untouched() {
    assert_eq!(guard(async { Ok(7) }).await.unwrap(), 7);
    assert!(current().is_none());
  }
}
