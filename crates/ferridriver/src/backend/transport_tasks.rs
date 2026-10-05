use futures::{Sink, SinkExt};
use tokio::sync::{Mutex, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::error::{FerriError, Result};

pub(crate) struct TransportTasks {
  shutdown: watch::Sender<bool>,
  tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl TransportTasks {
  pub(crate) fn new(shutdown: watch::Sender<bool>, tasks: Vec<JoinHandle<()>>) -> Self {
    Self {
      shutdown,
      tasks: Mutex::new(tasks),
    }
  }

  pub(crate) fn start_close(&self) {
    self.shutdown.send_replace(true);
  }

  pub(crate) async fn close(&self) -> Result<()> {
    self.start_close();
    let mut tasks = self.tasks.lock().await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut forced = false;
    let mut errors = Vec::new();
    while let Some(task) = tasks.last_mut() {
      let result = if forced {
        task.await
      } else if let Ok(result) = tokio::time::timeout_at(deadline, task).await {
        result
      } else {
        tracing::debug!("Transport close grace period expired; cancelling I/O tasks");
        for task in tasks.iter() {
          task.abort();
        }
        forced = true;
        continue;
      };
      tasks.pop();
      if let Err(error) = result
        && !error.is_cancelled()
      {
        tracing::warn!(%error, "Transport task failed during cleanup");
        errors.push(error.to_string());
      }
    }
    if errors.is_empty() {
      Ok(())
    } else {
      Err(FerriError::backend(format!(
        "Transport cleanup failed: {}",
        errors.join("; ")
      )))
    }
  }
}

pub(crate) async fn write_websocket<S>(
  mut writer: S,
  mut messages: mpsc::Receiver<Message>,
  mut stopping: watch::Receiver<bool>,
) where
  S: Sink<Message> + Unpin,
  S::Error: std::fmt::Display,
{
  tokio::select! {
    biased;
    _ = stopping.wait_for(|stopping| *stopping) => {},
    () = async {
      while let Some(message) = messages.recv().await {
        if let Err(error) = writer.send(message).await {
          tracing::debug!(%error, "WebSocket write failed");
          break;
        }
      }
    } => {},
  }
  drop(messages);
  if let Err(error) = writer.close().await {
    tracing::debug!(%error, "WebSocket writer closed without a complete handshake");
  }
}

impl Drop for TransportTasks {
  fn drop(&mut self) {
    self.start_close();
    for task in self.tasks.get_mut() {
      task.abort();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  struct Released(Arc<AtomicUsize>);

  impl Drop for Released {
    fn drop(&mut self) {
      self.0.fetch_add(1, Ordering::SeqCst);
    }
  }

  async fn held_tasks(released: &Arc<AtomicUsize>) -> (TransportTasks, watch::Sender<bool>) {
    let (release, receiver) = watch::channel(false);
    let mut handles = Vec::new();
    for _ in 0..2 {
      let mut receiver = receiver.clone();
      let resource = Released(Arc::clone(released));
      let (started, ready) = tokio::sync::oneshot::channel();
      handles.push(tokio::spawn(async move {
        let _resource = resource;
        started.send(()).unwrap();
        receiver.wait_for(|released| *released).await.unwrap();
      }));
      ready.await.unwrap();
    }
    (TransportTasks::new(watch::channel(false).0, handles), release)
  }

  #[tokio::test]
  async fn cancelled_close_retains_workers_and_retry_waits_for_their_resources() {
    let released = Arc::new(AtomicUsize::new(0));
    let (tasks, release) = held_tasks(&released).await;
    let mut closing = Box::pin(tasks.close());
    assert!(futures::poll!(&mut closing).is_pending());
    drop(closing);
    assert_eq!(tasks.tasks.lock().await.len(), 2);
    assert_eq!(released.load(Ordering::SeqCst), 0);
    let mut retry = Box::pin(tasks.close());
    assert!(futures::poll!(&mut retry).is_pending());
    release.send_replace(true);
    retry.await.unwrap();
    assert_eq!(released.load(Ordering::SeqCst), 2);
    assert!(tasks.tasks.lock().await.is_empty());
    tasks.close().await.unwrap();
  }

  #[tokio::test(start_paused = true)]
  async fn expired_grace_period_aborts_and_awaits_both_workers() {
    let released = Arc::new(AtomicUsize::new(0));
    let (tasks, _release) = held_tasks(&released).await;
    let mut closing = Box::pin(tasks.close());
    assert!(futures::poll!(&mut closing).is_pending());
    tokio::time::advance(std::time::Duration::from_secs(5)).await;
    closing.await.unwrap();
    assert_eq!(released.load(Ordering::SeqCst), 2);
    assert!(tasks.tasks.lock().await.is_empty());
  }

  #[tokio::test]
  async fn dropping_the_owner_aborts_retained_workers() {
    let released = Arc::new(AtomicUsize::new(0));
    let (tasks, _release) = held_tasks(&released).await;
    let handles: Vec<_> = tasks.tasks.lock().await.iter().map(JoinHandle::abort_handle).collect();
    drop(tasks);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
      while handles.iter().any(|handle| !handle.is_finished()) {
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
    assert_eq!(released.load(Ordering::SeqCst), 2);
  }
}
