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

/// Work item pulled by a worker.
pub enum WorkItem {
  /// Single parallel test.
  Single(TestAssignment),
  /// Batch of serial tests (one worker runs all, in order).
  Serial(SerialBatch),
}

/// Dispatch strategy: parallel tests via shared MPMC channel,
/// serial suites as batches on the same channel.
pub struct Dispatcher {
  tx: async_channel::Sender<WorkItem>,
  rx: async_channel::Receiver<WorkItem>,
}

impl Dispatcher {
  pub fn new() -> Self {
    let (tx, rx) = async_channel::unbounded();
    Self { tx, rx }
  }

  /// Enqueue a single parallel test.
  pub async fn enqueue_single(&self, assignment: TestAssignment) {
    let _ = self.tx.send(WorkItem::Single(assignment)).await;
  }

  /// Enqueue an entire serial suite as a batch.
  pub async fn enqueue_serial(&self, batch: SerialBatch) {
    let _ = self.tx.send(WorkItem::Serial(batch)).await;
  }

  /// Re-enqueue a test for retry (always as single item).
  pub async fn retry_shared(
    &self,
    test_fn: &TestFn,
    id: &TestId,
    fixture_requests: Vec<String>,
    attempt: u32,
    suite_key: String,
    hooks: Arc<Hooks>,
  ) {
    let assignment = TestAssignment {
      test: TestCase {
        id: id.clone(),
        test_fn: Arc::clone(test_fn),
        fixture_requests,
        annotations: Vec::new(),
        timeout: None,
        retries: None,
        expected_status: crate::model::ExpectedStatus::Pass,
      },
      attempt,
      suite_key,
      hooks,
      suite_mode: SuiteMode::Parallel,
    };
    let _ = self.tx.send(WorkItem::Single(assignment)).await;
  }

  /// Get a receiver clone for a worker.
  pub fn receiver(&self) -> async_channel::Receiver<WorkItem> {
    self.rx.clone()
  }

  /// Signal no more work will be enqueued.
  pub fn close(&self) {
    self.tx.close();
  }
}

impl Default for Dispatcher {
  fn default() -> Self {
    Self::new()
  }
}
