use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use crate::{FerriError, Result};

tokio::task_local! {
  static CURRENT: Option<OperationBudget>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct OperationBudget {
  deadline: crate::pause::Deadline,
  timeout_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Expired {
  pub(crate) timeout_ms: u64,
}

impl Expired {
  pub(crate) fn error(self, operation: impl Into<String>) -> FerriError {
    FerriError::timeout(operation, self.timeout_ms)
  }
}

impl OperationBudget {
  pub(crate) fn new(timeout_ms: u64) -> Result<Self> {
    Self::create(timeout_ms, Some(crate::pause::pause_clock()))
  }

  pub(crate) fn wall_clock(timeout_ms: u64) -> Result<Self> {
    Self::create(timeout_ms, None)
  }

  fn create(timeout_ms: u64, clock: Option<&'static crate::pause::PauseClock>) -> Result<Self> {
    let deadline = if timeout_ms == 0 {
      None
    } else {
      Some(
        Instant::now()
          .checked_add(Duration::from_millis(timeout_ms))
          .ok_or_else(|| FerriError::invalid_argument("timeout", "operation timeout is too large"))?,
      )
    };
    Ok(Self {
      deadline: crate::pause::Deadline::new(
        clock,
        deadline,
        clock.map_or(Duration::ZERO, crate::pause::PauseClock::parked_now),
      ),
      timeout_ms,
    })
  }

  pub(crate) fn operation(timeout_ms: u64) -> Result<Self> {
    let requested = Self::new(timeout_ms)?;
    Ok(match (Self::capture(), requested.effective_deadline()) {
      (Some(parent), Some(deadline)) if parent.effective_deadline().is_some_and(|limit| limit <= deadline) => parent,
      (Some(parent), None) => parent,
      _ => requested,
    })
  }

  pub(crate) fn capture() -> Option<Self> {
    CURRENT.try_with(|budget| *budget).ok().flatten()
  }

  pub(crate) fn current(default_ms: u64) -> Result<Self> {
    Self::capture().map_or_else(|| Self::new(default_ms), Ok)
  }

  pub(crate) fn scope<F: Future>(self, future: F) -> impl Future<Output = F::Output> {
    CURRENT.scope(Some(self), future)
  }

  pub(crate) fn timeout_ms(self) -> u64 {
    self.timeout_ms
  }

  fn effective_deadline(self) -> Option<Instant> {
    self.deadline.effective()
  }

  pub(crate) fn remaining_ms(self) -> std::result::Result<Option<u64>, Expired> {
    self
      .effective_deadline()
      .map(|deadline| {
        let remaining = deadline
          .checked_duration_since(Instant::now())
          .filter(|remaining| !remaining.is_zero())
          .ok_or(Expired {
            timeout_ms: self.timeout_ms,
          })?;
        Ok(u64::try_from(remaining.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX))
      })
      .transpose()
  }

  pub(crate) fn wait<F: Future>(&self, future: F) -> impl Future<Output = std::result::Result<F::Output, Expired>> {
    use futures::TryFutureExt as _;
    let timeout_ms = self.timeout_ms;
    crate::pause::with_deadline(&self.deadline, future).map_err(move |_| Expired { timeout_ms })
  }

  pub(crate) async fn send<T>(&self, sender: &tokio::sync::mpsc::Sender<T>, value: T) -> Result<()> {
    use tokio::sync::mpsc::error::TrySendError;
    self
      .remaining_ms()
      .map_err(|error| error.error("queueing protocol command"))?;
    let permit = match sender.try_reserve() {
      Ok(permit) => permit,
      Err(TrySendError::Closed(())) => return Err(FerriError::target_closed(Some("Protocol writer closed".into()))),
      Err(TrySendError::Full(())) => {
        // Keep the semaphore waiter off every caller's stack; only saturated queues allocate it.
        Box::pin(self.wait(sender.reserve()))
          .await
          .map_err(|error| error.error("queueing protocol command"))?
          .map_err(|_| FerriError::target_closed(Some("Protocol writer closed".into())))?
      },
    };
    self
      .remaining_ms()
      .map_err(|error| error.error("queueing protocol command"))?;
    permit.send(value);
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test(start_paused = true)]
  async fn queue_capacity_preserves_order_and_releases_cancelled_reservations() {
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let budget = OperationBudget::new(1000).unwrap();
    budget.send(&sender, 1).await.unwrap();
    let mut cancelled = Box::pin(budget.send(&sender, 2));
    assert!(futures::poll!(&mut cancelled).is_pending());
    drop(cancelled);
    let mut waiting = Box::pin(budget.send(&sender, 3));
    assert!(futures::poll!(&mut waiting).is_pending());
    assert_eq!(receiver.recv().await, Some(1));
    waiting.await.unwrap();
    assert_eq!(receiver.recv().await, Some(3));
    tokio::time::advance(Duration::from_millis(1000)).await;
    assert!(budget.send(&sender, 4).await.unwrap_err().is_timeout_error());
    assert!(receiver.try_recv().is_err());
    receiver.close();
    assert!(matches!(
      OperationBudget::new(0).unwrap().send(&sender, 5).await,
      Err(FerriError::TargetClosed { .. })
    ));
  }

  #[tokio::test]
  async fn debugger_parks_suspend_shared_budgets_without_extending_nested_or_cleanup_limits() {
    crate::pause::with_test_clock(async {
      let budget = OperationBudget::new(30).unwrap();
      let cleanup = OperationBudget::wall_clock(30).unwrap();
      budget
        .wait(async {
          let park = crate::pause::pause_clock().park();
          tokio::time::sleep(Duration::from_millis(80)).await;
          drop(park);
        })
        .await
        .unwrap();
      assert!(cleanup.remaining_ms().is_err());
      budget
        .scope(async {
          assert_eq!(OperationBudget::operation(5).unwrap().timeout_ms(), 5);
        })
        .await;
      assert!(
        budget
          .wait(tokio::time::sleep(Duration::from_millis(80)))
          .await
          .is_err()
      );
    })
    .await;
  }

  #[tokio::test(start_paused = true)]
  async fn unlimited_scope_overrides_transport_defaults_and_nested_operations_cannot_extend_a_parent() {
    OperationBudget::new(0)
      .unwrap()
      .scope(async {
        let command = OperationBudget::current(30_000).unwrap();
        command
          .wait(tokio::time::sleep(Duration::from_secs(120)))
          .await
          .unwrap();
        assert_eq!(command.remaining_ms().unwrap(), None);
      })
      .await;
    OperationBudget::new(1000)
      .unwrap()
      .scope(async {
        tokio::time::sleep(Duration::from_millis(700)).await;
        for timeout in [0, 30_000] {
          let child = OperationBudget::operation(timeout).unwrap();
          assert_eq!(child.remaining_ms().unwrap(), Some(300));
        }
        let child = OperationBudget::operation(100).unwrap();
        assert_eq!(child.remaining_ms().unwrap(), Some(100));
      })
      .await;
    assert!(OperationBudget::capture().is_none());
  }

  #[tokio::test(start_paused = true)]
  async fn sequential_replies_share_one_absolute_deadline() {
    let budget = OperationBudget::new(1000).unwrap();
    budget
      .wait(tokio::time::sleep(Duration::from_millis(700)))
      .await
      .unwrap();
    let started = Instant::now();
    let error = budget
      .wait(tokio::time::sleep(Duration::from_millis(700)))
      .await
      .unwrap_err();
    assert_eq!(error.timeout_ms, 1000);
    assert_eq!(Instant::now() - started, Duration::from_millis(300));
    let mut invoked = false;
    assert!(budget.wait(async { invoked = true }).await.is_err());
    assert!(!invoked);
  }
}
