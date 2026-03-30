//! Test dispatcher: MPMC work queue with longest-first scheduling.

use std::sync::Arc;

use crate::model::{TestCase, TestFn, TestId};

/// A test assigned to a worker with its attempt number.
pub struct TestAssignment {
  /// The test to run.
  pub test: TestCase,
  /// Current attempt number (1-indexed).
  pub attempt: u32,
}

/// Retry info: enough data to re-create a `TestAssignment` without cloning TestCase.
pub struct RetryAssignment {
  pub id: TestId,
  pub test_fn: TestFn,
  pub fixture_requests: Vec<String>,
  pub attempt: u32,
  pub timeout: Option<std::time::Duration>,
  pub retries: Option<u32>,
}

/// Dispatch strategy: tests are fed into a shared MPMC channel.
/// Workers pull from this channel, achieving natural load balancing.
pub struct Dispatcher {
  tx: async_channel::Sender<TestAssignment>,
  rx: async_channel::Receiver<TestAssignment>,
}

impl Dispatcher {
  pub fn new() -> Self {
    let (tx, rx) = async_channel::unbounded();
    Self { tx, rx }
  }

  /// Enqueue all tests (takes ownership).
  pub async fn enqueue_all(&self, tests: Vec<TestCase>) {
    for test in tests {
      let _ = self.tx.send(TestAssignment { test, attempt: 1 }).await;
    }
  }

  /// Enqueue tests by reference (Arc-based TestFn allows sharing).
  /// Used for repeatEach where the same tests are enqueued multiple times.
  pub async fn enqueue_all_shared(&self, tests: &[TestCase]) {
    for test in tests {
      let assignment = TestAssignment {
        test: TestCase {
          id: test.id.clone(),
          test_fn: Arc::clone(&test.test_fn),
          fixture_requests: test.fixture_requests.clone(),
          annotations: test.annotations.clone(),
          timeout: test.timeout,
          retries: test.retries,
          expected_status: test.expected_status.clone(),
        },
        attempt: 1,
      };
      let _ = self.tx.send(assignment).await;
    }
  }

  /// Re-enqueue a test for retry using saved test info from the worker result.
  pub async fn retry_shared(
    &self,
    test_fn: &TestFn,
    id: &TestId,
    fixture_requests: Vec<String>,
    attempt: u32,
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
    };
    let _ = self.tx.send(assignment).await;
  }

  /// Get a receiver clone for a worker.
  pub fn receiver(&self) -> async_channel::Receiver<TestAssignment> {
    self.rx.clone()
  }

  /// Signal no more tests will be enqueued.
  pub fn close(&self) {
    self.tx.close();
  }
}

impl Default for Dispatcher {
  fn default() -> Self {
    Self::new()
  }
}
