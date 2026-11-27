//! Unit tests for the lossless ordered event emitter.

use super::*;
use std::sync::atomic::AtomicUsize;

#[tokio::test]
async fn a_zero_timeout_waits_without_a_limit() {
  let emitter = EventEmitter::new();
  let waiting = emitter.wait_for_event("load", 0);
  let late = emitter.clone();
  tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    late.emit(PageEvent::Load);
  });
  assert!(matches!(waiting.await, Ok(PageEvent::Load)));
}

#[tokio::test]
async fn late_listeners_and_subscribers_do_not_receive_queued_history() {
  let emitter = EventEmitter::new();
  emitter.emit(PageEvent::Load);
  let mut subscription = emitter.subscribe();
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = Arc::clone(&calls);
  emitter.once(
    "load",
    Arc::new(move |_| {
      observed.fetch_add(1, Ordering::Relaxed);
    }),
  );
  emitter.emit(PageEvent::Close);
  let event = tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(matches!(event, PageEvent::Close));
  assert_eq!(calls.load(Ordering::Relaxed), 0);
  emitter.emit(PageEvent::Load);
  subscription
    .drain_until(|event| matches!(event, PageEvent::Load), 1000)
    .await
    .unwrap();
  assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn forwarding_keeps_the_original_event_registration_boundary() {
  let source = EventEmitter::new();
  let destination = EventEmitter::new();
  let mut bridge = source.subscribe();
  source.emit(PageEvent::Load);
  let mut subscription = destination.subscribe();
  let calls = Arc::new(AtomicUsize::new(0));
  let observed = Arc::clone(&calls);
  destination.once(
    "load",
    Arc::new(move |_| {
      observed.fetch_add(1, Ordering::Relaxed);
    }),
  );
  let (event, sequence) = tokio::time::timeout(std::time::Duration::from_secs(1), bridge.recv_stamped())
    .await
    .unwrap()
    .unwrap();
  destination.emit_stamped(event, sequence);
  destination.emit(PageEvent::Close);
  let event = tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
    .await
    .unwrap()
    .unwrap();
  assert!(matches!(event, PageEvent::Close));
  assert_eq!(calls.load(Ordering::Relaxed), 0);
  destination.emit(PageEvent::Load);
  subscription
    .drain_until(|event| matches!(event, PageEvent::Load), 1000)
    .await
    .unwrap();
  assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn state_updates_precede_listeners_and_raw_subscribers() -> Result<(), Box<dyn std::error::Error>> {
  let emitter = EventEmitter::new();
  let state = Arc::new(AtomicUsize::new(0));
  let observed = Arc::clone(&state);
  assert!(emitter.set_state_observer(Arc::new(move |_| {
    observed.fetch_add(1, Ordering::SeqCst);
  })));
  assert!(!emitter.set_state_observer(Arc::new(|_| {})));
  emitter.remove_all_listeners();
  let (tx, mut delivered) = mpsc::unbounded_channel();
  let observed = Arc::clone(&state);
  emitter.prepend_once(
    "load",
    Arc::new(move |_| {
      let _ = tx.send(observed.load(Ordering::SeqCst));
    }),
  );
  let mut subscription = emitter.subscribe();
  emitter.emit(PageEvent::Load);
  let value = tokio::time::timeout(std::time::Duration::from_secs(1), delivered.recv()).await?;
  assert_eq!(value, Some(1));
  assert!(matches!(subscription.recv().await, Some(PageEvent::Load)));
  assert_eq!(state.load(Ordering::SeqCst), 1);
  Ok(())
}

fn counting(counter: &Arc<AtomicUsize>) -> EventCallback {
  let counter = Arc::clone(counter);
  Arc::new(move |_| {
    counter.fetch_add(1, Ordering::SeqCst);
  })
}

async fn settle(counter: &Arc<AtomicUsize>, expect: usize) {
  for _ in 0..200 {
    if counter.load(Ordering::SeqCst) >= expect {
      return;
    }
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn storm_is_lossless() {
  let emitter = EventEmitter::new();
  let counter = Arc::new(AtomicUsize::new(0));
  emitter.on("load", counting(&counter));
  for _ in 0..10_000 {
    emitter.emit(PageEvent::Load);
  }
  settle(&counter, 10_000).await;
  assert_eq!(counter.load(Ordering::SeqCst), 10_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listeners_fire_in_registration_order() {
  let emitter = EventEmitter::new();
  let order = Arc::new(std::sync::Mutex::new(Vec::new()));
  for tag in 0..8usize {
    let order = Arc::clone(&order);
    emitter.on(
      "load",
      Arc::new(move |_| {
        lock_or_recover(&order).push(tag);
      }),
    );
  }
  for _ in 0..100 {
    emitter.emit(PageEvent::Load);
  }
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
  while lock_or_recover(&order).len() < 800 && tokio::time::Instant::now() < deadline {
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
  }
  let seen = lock_or_recover(&order);
  assert_eq!(seen.len(), 800);
  for chunk in seen.chunks(8) {
    assert_eq!(chunk, &[0, 1, 2, 3, 4, 5, 6, 7]);
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn once_fires_exactly_once() {
  let emitter = EventEmitter::new();
  let counter = Arc::new(AtomicUsize::new(0));
  emitter.once("load", counting(&counter));
  for _ in 0..50 {
    emitter.emit(PageEvent::Load);
  }
  settle(&counter, 1).await;
  tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  assert_eq!(counter.load(Ordering::SeqCst), 1);
  assert!(!emitter.has_listener("load"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn off_removes_listener() {
  let emitter = EventEmitter::new();
  let counter = Arc::new(AtomicUsize::new(0));
  let id = emitter.on("load", counting(&counter));
  emitter.emit(PageEvent::Load);
  settle(&counter, 1).await;
  emitter.off(id);
  emitter.emit(PageEvent::Load);
  tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_for_subscribes_synchronously() {
  let emitter = EventEmitter::new();
  // Emit immediately after wait_for returns the future, BEFORE the
  // future is polled. The old emitter subscribed on first poll and
  // missed this event.
  let fut = emitter.wait_for_event("load", 1_000);
  emitter.emit(PageEvent::Load);
  let got = fut.await;
  assert!(got.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_is_lossless_and_ordered() {
  let emitter = EventEmitter::new();
  let mut sub = emitter.subscribe();
  for _ in 0..1_000 {
    emitter.emit(PageEvent::Load);
  }
  emitter.emit(PageEvent::DomContentLoaded);
  let mut loads = 0;
  loop {
    match sub.recv().await {
      Some(PageEvent::Load) => loads += 1,
      Some(PageEvent::DomContentLoaded) => break,
      other => {
        drop(other);
        break;
      },
    }
  }
  assert_eq!(loads, 1_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_closes_on_emitter_drop() {
  let emitter = EventEmitter::new();
  let mut sub = emitter.subscribe();
  drop(emitter);
  assert!(sub.recv().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callback_can_reenter_emitter() {
  let emitter = EventEmitter::new();
  let counter = Arc::new(AtomicUsize::new(0));
  let em2 = emitter.clone();
  let counter2 = Arc::clone(&counter);
  emitter.on(
    "load",
    Arc::new(move |_| {
      // Reentrant registration + emit from inside a callback must not
      // deadlock the dispatcher.
      em2.once("domcontentloaded", counting(&counter2));
      em2.emit(PageEvent::DomContentLoaded);
    }),
  );
  emitter.emit(PageEvent::Load);
  settle(&counter, 1).await;
  assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_emitter_stays_lazy_and_resumes_on_attach_detach_cycles() {
  let emitter = EventEmitter::new();
  // Emitting without any consumer must not spawn the dispatcher task.
  emitter.emit(PageEvent::Load);
  assert!(!emitter.inner.dispatcher_running.load(Ordering::Acquire));
  assert_eq!(emitter.inner.shared.consumers.load(Ordering::SeqCst), 0);

  let sub1 = emitter.subscribe();
  let mut sub2 = emitter.subscribe();
  assert!(emitter.inner.dispatcher_running.load(Ordering::Acquire));
  assert_eq!(emitter.inner.shared.consumers.load(Ordering::SeqCst), 2);
  drop(sub1);
  assert_eq!(emitter.inner.shared.consumers.load(Ordering::SeqCst), 1);

  emitter.emit(PageEvent::DomContentLoaded);
  assert!(matches!(
    tokio::time::timeout(std::time::Duration::from_secs(1), sub2.recv()).await,
    Ok(Some(PageEvent::DomContentLoaded))
  ));
  drop(sub2);
  assert_eq!(emitter.inner.shared.consumers.load(Ordering::SeqCst), 0);

  // Re-attaching after returning to 0 consumers delivers subsequent events cleanly.
  let counter = Arc::new(AtomicUsize::new(0));
  let id = emitter.once("load", counting(&counter));
  assert_eq!(emitter.inner.shared.consumers.load(Ordering::SeqCst), 1);
  emitter.emit(PageEvent::Load);
  settle(&counter, 1).await;
  assert_eq!(counter.load(Ordering::SeqCst), 1);
  assert_eq!(emitter.inner.shared.consumers.load(Ordering::SeqCst), 0);
  // Double off is a no-op and must not underflow consumers.
  emitter.off(id);
  assert_eq!(emitter.inner.shared.consumers.load(Ordering::SeqCst), 0);
}
