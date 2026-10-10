//! Test dispatcher: MPMC work queue supporting parallel and serial suites.
//!
//! Parallel tests go to a shared channel (natural load balancing).
//! Serial suites go as batches — one worker picks up the entire suite.

use std::sync::Arc;

use crate::model::{Hooks, SuiteMode, TestCase, TestFn, TestId};

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
pub struct Retry<'a> {
  pub test_fn: &'a TestFn,
  pub id: &'a TestId,
  pub fixture_requests: Vec<String>,
  pub attempt: u32,
  pub suite_key: String,
  pub hooks: Arc<Hooks>,
  pub metadata: Option<serde_json::Value>,
  pub locks: Vec<crate::model::TestAnnotation>,
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

  /// Re-enqueue a test for retry (always as single item).
  pub fn retry_shared(&self, retry: Retry<'_>) {
    let assignment = TestAssignment {
      test: TestCase {
        metadata: retry.metadata,
        id: retry.id.clone(),
        test_fn: Arc::clone(retry.test_fn),
        fixture_requests: retry.fixture_requests,
        // A retry still holds the test's locks; nothing else on the list
        // schedules it.
        annotations: retry.locks,
        timeout: None,
        retries: None,
        expected_status: crate::model::ExpectedStatus::Pass,
        use_options: None,
      },
      attempt: retry.attempt,
      suite_key: retry.suite_key,
      hooks: retry.hooks,
      suite_mode: SuiteMode::Parallel,
    };
    let _ = self.tx.try_send(WorkItem::Single(assignment));
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
