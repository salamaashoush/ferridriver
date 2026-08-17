//! Soft assertions: a failure that is recorded and lets the test carry
//! on, failing it at the end.
//!
//! The rule is here, in core, so both hosts obey one definition: a
//! failure raised by a `.soft()` assertion is offered to whatever sink
//! the current test installed, and only becomes an error the caller sees
//! if there is no sink to take it — outside a test, a soft assertion
//! fails loudly rather than vanishing.
//!
//! The sink itself is per-host: the Rust runner installs one backed by
//! the test's `TestInfo` around the test body, and the QuickJS binding
//! routes through the per-test host bridge it already holds.

use std::sync::Arc;

use crate::AssertionFailure;

/// Where a soft failure goes. Recording must not block: it runs inside
/// a matcher, on whatever task the test body is using.
pub trait SoftSink: Send + Sync {
  fn record(&self, failure: &AssertionFailure);
}

tokio::task_local! {
  static CURRENT_SINK: Arc<dyn SoftSink>;
}

/// Run `fut` with `sink` collecting the soft failures raised inside it.
///
/// Scoped to the task, so a soft assertion in a detached `tokio::spawn`
/// is deliberately NOT part of the test that spawned it.
pub async fn with_sink<F: std::future::Future>(sink: Arc<dyn SoftSink>, fut: F) -> F::Output {
  CURRENT_SINK.scope(sink, fut).await
}

/// Offer a failure to the current sink. `false` when there is none, in
/// which case the caller must surface the failure normally.
#[must_use]
pub fn record(failure: &AssertionFailure) -> bool {
  CURRENT_SINK
    .try_with(|sink| {
      sink.record(failure);
    })
    .is_ok()
}

/// The one place the soft rule is applied: a soft failure a sink took
/// is not an error; anything else passes through unchanged.
pub fn absorb(failure: AssertionFailure) -> Result<(), AssertionFailure> {
  if failure.soft && record(&failure) {
    return Ok(());
  }
  Err(failure)
}

/// `absorb` for a matcher's result.
pub fn absorb_result(result: Result<(), AssertionFailure>) -> Result<(), AssertionFailure> {
  match result {
    Ok(()) => Ok(()),
    Err(failure) => absorb(failure),
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use super::*;

  #[derive(Default)]
  struct Collector(Mutex<Vec<String>>);

  impl SoftSink for Collector {
    fn record(&self, failure: &AssertionFailure) {
      self.0.lock().expect("lock").push(failure.message.clone());
    }
  }

  fn soft_failure(message: &str) -> AssertionFailure {
    AssertionFailure::new(message, None).as_soft()
  }

  #[tokio::test]
  async fn a_sink_takes_a_soft_failure_and_the_caller_continues() {
    let sink = Arc::new(Collector::default());
    let taken = Arc::clone(&sink);
    with_sink(sink, async move {
      absorb(soft_failure("first")).expect("a soft failure must not surface");
      absorb(soft_failure("second")).expect("a soft failure must not surface");
      assert_eq!(taken.0.lock().expect("lock").len(), 2);
    })
    .await;
  }

  #[tokio::test]
  async fn a_hard_failure_is_never_absorbed() {
    let sink = Arc::new(Collector::default());
    let seen = Arc::clone(&sink);
    with_sink(sink, async move {
      absorb(AssertionFailure::new("hard", None)).unwrap_err();
      assert!(seen.0.lock().expect("lock").is_empty());
    })
    .await;
  }

  #[tokio::test]
  async fn a_soft_value_matcher_records_and_returns_ok() {
    let sink = Arc::new(Collector::default());
    let taken = Arc::clone(&sink);
    with_sink(sink, async move {
      // The whole point: the caller can `?` a soft assertion and keep going.
      crate::expect_value(serde_json::json!(1))
        .soft()
        .to_be(&serde_json::json!(2))
        .expect("a soft assertion must not surface as an error");
      // A hard one on the same value still stops the caller.
      crate::expect_value(serde_json::json!(1))
        .to_be(&serde_json::json!(2))
        .unwrap_err();
      let recorded = taken.0.lock().expect("lock");
      assert_eq!(recorded.len(), 1, "only the soft failure is recorded");
      assert!(recorded[0].contains("toBe"), "{}", recorded[0]);
    })
    .await;
  }

  #[tokio::test]
  async fn a_soft_custom_matcher_records_too() {
    let sink = Arc::new(Collector::default());
    let taken = Arc::clone(&sink);
    with_sink(sink, async move {
      let m = crate::extend::matcher(
        |_cx: &crate::MatcherContext, _actual: &serde_json::Value, _args: &[_]| {
          crate::MatcherResult::new(false).with_message("nope")
        },
      );
      crate::expect_value(serde_json::json!(1))
        .soft()
        .matches("toBeX", &m, &[])
        .expect("a soft custom matcher must not surface");
      assert_eq!(taken.0.lock().expect("lock").as_slice(), ["nope"]);
    })
    .await;
  }

  #[tokio::test]
  async fn without_a_sink_a_soft_failure_still_fails() {
    // Outside a test there is nothing to collect it, and swallowing it
    // would turn a failed assertion into silence.
    absorb(soft_failure("orphan")).unwrap_err();
  }
}
