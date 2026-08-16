//! Time the process spends parked at the debugger, and deadlines that do
//! not run while it is.
//!
//! `test --debug` holds a run in front of an API call for as long as a
//! person is reading the page. Every deadline that would otherwise fire
//! meanwhile has to stand still: the test's own timeout, the script
//! engine's per-call timeout, and the backstop around it. They live in
//! different crates, so the clock lives here — below all of them, beside
//! the [`ActionGate`] that does the parking.
//!
//! Suspending, never disabling. A test that hangs on its own after being
//! released still times out, which is what makes `--debug` safe to leave
//! on while chasing something else. Playwright suspends the same way
//! (`testInfo._setIgnoreTimeouts` around a paused context).
//!
//! [`ActionGate`]: crate::trace::ActionGate

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long the process has spent parked at the debugger.
pub struct PauseClock {
  /// Accumulated parked time, in milliseconds. Parks are strictly
  /// sequential (`--debug` runs one worker), so a counter is enough.
  parked_ms: AtomicU64,
  /// Start of the park in progress, if any.
  since: Mutex<Option<Instant>>,
}

impl PauseClock {
  fn new() -> Self {
    Self {
      parked_ms: AtomicU64::new(0),
      since: Mutex::new(None),
    }
  }

  /// Start counting a park. The park ends when the guard drops.
  #[must_use]
  pub fn park(&'static self) -> ParkGuard {
    *self.since.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
    ParkGuard(self)
  }

  /// Total time parked so far, counting only parks that have ended.
  #[must_use]
  pub fn parked(&self) -> Duration {
    Duration::from_millis(self.parked_ms.load(Ordering::Acquire))
  }

  /// Total time parked so far, including the park in progress.
  ///
  /// What a live deadline wants: while a park is open this grows with the
  /// wall clock, so a deadline computed from it stands still relative to
  /// the work rather than creeping toward firing.
  #[must_use]
  pub fn parked_now(&self) -> Duration {
    let open = self
      .since
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .map(|since| since.elapsed())
      .unwrap_or_default();
    self.parked() + open
  }

  /// Whether a park is in progress.
  #[must_use]
  pub fn is_parked(&self) -> bool {
    self
      .since
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .is_some()
  }
}

/// Ends the park it was created for.
pub struct ParkGuard(&'static PauseClock);

impl Drop for ParkGuard {
  fn drop(&mut self) {
    let started = self
      .0
      .since
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .take();
    if let Some(started) = started {
      let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
      self.0.parked_ms.fetch_add(ms, Ordering::AcqRel);
    }
  }
}

/// The process's parked clock.
pub fn pause_clock() -> &'static PauseClock {
  static CLOCK: OnceLock<PauseClock> = OnceLock::new();
  CLOCK.get_or_init(PauseClock::new)
}

/// A deadline ran out. Distinct from [`tokio::time::error::Elapsed`]
/// because this deadline is not a plain sleep — see [`run_within`].
#[derive(Debug)]
pub struct Timedout;

impl std::fmt::Display for Timedout {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("deadline exceeded")
  }
}

impl std::error::Error for Timedout {}

/// Run `fut` under a deadline that does not advance while the process is
/// parked at the debugger.
///
/// # Errors
///
/// [`Timedout`] when `limit` elapses without the future finishing, not
/// counting time parked.
pub async fn run_within<F: Future>(limit: Duration, fut: F) -> Result<F::Output, Timedout> {
  /// How often to look again while parked. The deadline moves with the
  /// wall clock meanwhile, so waking is only to re-arm the timer.
  const PARK_TICK: Duration = Duration::from_millis(100);

  let clock = pause_clock();
  let started = Instant::now();
  // The clock counts the whole process, so only what it gains from here on
  // belongs to this call — otherwise work that runs after a long stop would
  // inherit that stop's grace and never time out.
  let parked_before = clock.parked_now();
  let deadline_now = || started + limit + clock.parked_now().saturating_sub(parked_before);
  let mut fut = std::pin::pin!(fut);
  loop {
    // `fut` stays in every select arm: the thing that ends a park is
    // usually inside it (the debugger's gate returns when a script
    // resumes it), so a branch that stopped polling `fut` while parked
    // would wait for a park only `fut` could end.
    let wake = if clock.is_parked() {
      Instant::now() + PARK_TICK
    } else {
      deadline_now()
    };
    tokio::select! {
      output = &mut fut => return Ok(output),
      () = tokio::time::sleep_until(wake.into()) => {
        // A park may have started, or started AND ended, inside the sleep;
        // either way it moves the deadline out from under it.
        if Instant::now() < deadline_now() {
          continue;
        }
        return Err(Timedout);
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::{pause_clock, run_within};

  // One test, not three: the clock is process-global, so separate `#[test]`
  // fns would run their parks concurrently and account each other's time.
  #[tokio::test(flavor = "multi_thread")]
  async fn parks_suspend_a_deadline_without_disabling_it() {
    let clock = pause_clock();

    // A park adds its own duration and nothing else, and while it is open
    // `parked()` does not move — only `parked_now()` does.
    let before = clock.parked();
    assert!(!clock.is_parked());
    let guard = clock.park();
    assert!(clock.is_parked());
    assert_eq!(clock.parked(), before);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(clock.parked_now() >= before + Duration::from_millis(25));
    drop(guard);
    assert!(!clock.is_parked());
    assert!(clock.parked() >= before + Duration::from_millis(25));

    // A park longer than the whole budget does not consume it, but real
    // work after the release still does.
    let work = async {
      let guard = clock.park();
      tokio::time::sleep(Duration::from_millis(120)).await;
      drop(guard);
      tokio::time::sleep(Duration::from_millis(400)).await;
    };
    assert!(
      run_within(Duration::from_millis(60), work).await.is_err(),
      "work that ran past its budget after being released must still time out"
    );

    // The park is ended by the future itself, which is the shape the
    // debugger's gate actually has: it parks and waits for a script to
    // resume it. A `run_within` that stopped polling `fut` while parked
    // would wait forever for a park only `fut` could end.
    let (tx, mut rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(150)).await;
      tx.send_replace(true);
    });
    let gated = async {
      let _parked = clock.park();
      let _ = rx.changed().await;
    };
    assert!(
      run_within(Duration::from_millis(30), gated).await.is_ok(),
      "a park ended from inside the future must not deadlock its own deadline"
    );
  }
}
