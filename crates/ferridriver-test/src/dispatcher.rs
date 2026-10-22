//! Test dispatcher: MPMC work queue supporting parallel and serial suites.
//!
//! Parallel tests go to a shared channel (natural load balancing).
//! Serial suites go as batches — one worker picks up the entire suite.

use std::sync::Arc;

use crate::model::{Hooks, SuiteMode, TestCase};

/// A single test assigned to a worker.
pub struct TestAssignment {
  pub test: TestCase,
  pub attempt: u32,
  /// Suite key for hook tracking (e.g. "file.rs::suite_name").
  pub suite_key: String,
  /// Shared hooks for this test's suite.
  pub hooks: Arc<Hooks>,
  /// Suite execution mode.
  pub suite_mode: SuiteMode,
}

/// A batch of serial tests — all run on one worker, in order.
pub struct SerialBatch {
  pub suite_key: String,
  pub assignments: Vec<TestAssignment>,
  pub hooks: Arc<Hooks>,
}

/// One more attempt of a test that failed.
pub struct Retry {
  /// The case exactly as it was first scheduled.
  pub test: TestCase,
  pub attempt: u32,
  pub suite_key: String,
  pub hooks: Arc<Hooks>,
  pub suite_mode: SuiteMode,
  /// The rest of the serial group, which resumes after the retry.
  pub serial_tail: Vec<TestAssignment>,
}

/// Work item pulled by a worker.
pub enum WorkItem {
  /// Single parallel test.
  Single(TestAssignment),
  /// Batch of serial tests (one worker runs all, in order).
  Serial(SerialBatch),
}

impl WorkItem {
  /// Every lock the item's tests declared: the whole item holds them, as
  /// a serial suite runs on one worker from first test to last.
  pub(crate) fn lock_names(&self) -> Vec<String> {
    let assignments = match self {
      Self::Single(assignment) => std::slice::from_ref(assignment),
      Self::Serial(batch) => batch.assignments.as_slice(),
    };
    assignments
      .iter()
      .flat_map(|assignment| crate::locks::names(&assignment.test.annotations))
      .map(str::to_string)
      .collect()
  }

  pub(crate) fn repeat_each_index(&self) -> u32 {
    match self {
      Self::Single(assignment) => assignment.test.id.repeat_each_index,
      Self::Serial(batch) => batch
        .assignments
        .first()
        .map_or(0, |assignment| assignment.test.id.repeat_each_index),
    }
  }
}

/// Dispatch strategy: parallel tests via shared MPMC channel,
/// serial suites as batches on the same channel.
pub struct Dispatcher {
  tx: async_channel::Sender<WorkItem>,
  rx: async_channel::Receiver<WorkItem>,
  /// Hard-stop signal. When set, workers must drop their current `recv()`
  /// outcome rather than processing it — `close()` alone keeps already-buffered
  /// items in the queue, which means `--max-failures N` and `-x` would still
  /// run every test that was enqueued before the threshold tripped.
  stopped: Arc<std::sync::atomic::AtomicBool>,
}

impl Dispatcher {
  pub fn new() -> Self {
    let (tx, rx) = async_channel::unbounded();
    Self {
      tx,
      rx,
      stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    }
  }

  /// Enqueue a single parallel test.
  pub fn enqueue_single(&self, assignment: TestAssignment) {
    let _ = self.tx.try_send(WorkItem::Single(assignment));
  }

  /// Enqueue an entire serial suite as a batch.
  pub fn enqueue_serial(&self, batch: SerialBatch) {
    let _ = self.tx.try_send(WorkItem::Serial(batch));
  }

  /// Re-enqueue a failed test for another attempt with every setting it
  /// was scheduled with. A serial test goes back as a batch together with
  /// the rest of its group, so the group still runs in order on one worker.
  pub fn retry_shared(&self, retry: Retry) {
    let assignment = TestAssignment {
      test: retry.test,
      attempt: retry.attempt,
      suite_key: retry.suite_key.clone(),
      hooks: Arc::clone(&retry.hooks),
      suite_mode: retry.suite_mode,
    };
    let item = if retry.suite_mode == SuiteMode::Serial {
      let mut assignments = Vec::with_capacity(retry.serial_tail.len() + 1);
      assignments.push(assignment);
      assignments.extend(retry.serial_tail);
      WorkItem::Serial(SerialBatch {
        suite_key: retry.suite_key,
        assignments,
        hooks: retry.hooks,
      })
    } else {
      WorkItem::Single(assignment)
    };
    let _ = self.tx.try_send(item);
  }

  /// Get a receiver clone for a worker.
  pub fn receiver(&self) -> async_channel::Receiver<WorkItem> {
    self.rx.clone()
  }

  /// Signal no more work will be enqueued.
  pub fn close(&self) {
    self.tx.close();
  }

  /// Trip the hard-stop signal so workers drop any remaining queued items
  /// instead of processing them. Used by `--max-failures` / `-x`.
  pub fn stop(&self) {
    self.stopped.store(true, std::sync::atomic::Ordering::SeqCst);
    self.tx.close();
  }

  /// Worker-side check: should the next item be processed or dropped?
  pub fn is_stopped(&self) -> bool {
    self.stopped.load(std::sync::atomic::Ordering::SeqCst)
  }

  /// Worker-side handle to the same atomic flag (cheap clone of the Arc).
  pub fn stop_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
    Arc::clone(&self.stopped)
  }
}

impl Default for Dispatcher {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::model::{ExpectedStatus, TestAnnotation, TestId};

  fn case(name: &str) -> TestCase {
    TestCase {
      id: TestId {
        file: "a.spec.ts".into(),
        name: name.into(),
        ..TestId::default()
      },
      test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: vec!["page".into()],
      annotations: vec![
        TestAnnotation::Slow {
          reason: None,
          condition: None,
        },
        TestAnnotation::Lock("db".into()),
      ],
      timeout: Some(Duration::from_millis(400)),
      retries: Some(3),
      expected_status: ExpectedStatus::Fail,
      use_options: Some(serde_json::json!({ "locale": "de-DE" })),
      metadata: Some(serde_json::json!({ "keyword": "Scenario" })),
    }
  }

  fn assignment(name: &str, mode: SuiteMode) -> TestAssignment {
    TestAssignment {
      test: case(name),
      attempt: 1,
      suite_key: "a.spec.ts::group".into(),
      hooks: Arc::new(Hooks::default()),
      suite_mode: mode,
    }
  }

  #[test]
  fn a_retry_keeps_every_setting_the_test_was_scheduled_with() {
    let dispatcher = Dispatcher::new();
    dispatcher.retry_shared(Retry {
      test: case("flaky"),
      attempt: 2,
      suite_key: "a.spec.ts::group".into(),
      hooks: Arc::new(Hooks::default()),
      suite_mode: SuiteMode::Parallel,
      serial_tail: Vec::new(),
    });
    let Ok(WorkItem::Single(retried)) = dispatcher.receiver().try_recv() else {
      panic!("a parallel retry goes back alone");
    };
    assert_eq!(retried.attempt, 2);
    assert_eq!(retried.test.timeout, Some(Duration::from_millis(400)));
    assert_eq!(retried.test.retries, Some(3));
    assert_eq!(retried.test.expected_status, ExpectedStatus::Fail);
    assert_eq!(retried.test.use_options, Some(serde_json::json!({ "locale": "de-DE" })));
    assert_eq!(retried.test.annotations.len(), 2);
    assert_eq!(retried.test.fixture_requests, ["page"]);
  }

  #[test]
  fn a_serial_retry_takes_the_rest_of_its_group_with_it() {
    let dispatcher = Dispatcher::new();
    dispatcher.retry_shared(Retry {
      test: case("two"),
      attempt: 2,
      suite_key: "a.spec.ts::group".into(),
      hooks: Arc::new(Hooks::default()),
      suite_mode: SuiteMode::Serial,
      serial_tail: vec![assignment("three", SuiteMode::Serial)],
    });
    let Ok(WorkItem::Serial(batch)) = dispatcher.receiver().try_recv() else {
      panic!("a serial retry goes back as a batch");
    };
    let order: Vec<(&str, u32)> = batch
      .assignments
      .iter()
      .map(|a| (a.test.id.name.as_str(), a.attempt))
      .collect();
    assert_eq!(order, [("two", 2), ("three", 1)]);
    assert_eq!(batch.assignments[0].suite_mode, SuiteMode::Serial);
  }
}
