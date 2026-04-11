//! Tests for the EventBus fan-out architecture and ReporterDriver.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use ferridriver_test::reporter::{EventBus, EventBusBuilder, ReporterDriver, ReporterEvent, ReporterSet};

// ── EventBus fan-out tests ──

#[tokio::test]
async fn event_bus_delivers_to_single_subscriber() {
  let mut builder = EventBusBuilder::new();
  let sub = builder.subscribe();
  let bus = builder.build();

  bus
    .emit(ReporterEvent::RunStarted {
      total_tests: 5,
      num_workers: 2,
      metadata: serde_json::Value::Null,
    })
    .await;
  drop(bus);

  let mut rx = sub.rx;
  let event = rx.recv().await.unwrap();
  assert!(matches!(
    event,
    ReporterEvent::RunStarted {
      total_tests: 5,
      num_workers: 2,
      ..
    }
  ));
  assert!(rx.recv().await.is_none(), "channel should be closed after bus drop");
}

#[tokio::test]
async fn event_bus_delivers_to_multiple_subscribers() {
  let mut builder = EventBusBuilder::new();
  let sub1 = builder.subscribe();
  let sub2 = builder.subscribe();
  let sub3 = builder.subscribe();
  let bus = builder.build();

  bus
    .emit(ReporterEvent::RunStarted {
      total_tests: 10,
      num_workers: 4,
      metadata: serde_json::Value::Null,
    })
    .await;
  bus
    .emit(ReporterEvent::RunFinished {
      total: 10,
      passed: 8,
      failed: 1,
      skipped: 1,
      flaky: 0,
      duration: Duration::from_secs(5),
    })
    .await;
  drop(bus);

  // All three subscribers should receive both events.
  for (i, sub) in [sub1, sub2, sub3].into_iter().enumerate() {
    let mut rx = sub.rx;
    let e1 = rx.recv().await.unwrap();
    assert!(
      matches!(e1, ReporterEvent::RunStarted { total_tests: 10, .. }),
      "sub {i} missing RunStarted"
    );
    let e2 = rx.recv().await.unwrap();
    assert!(
      matches!(e2, ReporterEvent::RunFinished { total: 10, .. }),
      "sub {i} missing RunFinished"
    );
    assert!(rx.recv().await.is_none(), "sub {i} channel should be closed");
  }
}

#[tokio::test]
async fn event_bus_clone_shares_subscribers() {
  let mut builder = EventBusBuilder::new();
  let sub = builder.subscribe();
  let bus = builder.build();

  // Clone simulates what workers do.
  let bus_clone = bus.clone();
  bus_clone.emit(ReporterEvent::WorkerStarted { worker_id: 0 }).await;
  drop(bus_clone);

  // Original bus still alive — channel not closed.
  bus.emit(ReporterEvent::WorkerFinished { worker_id: 0 }).await;
  drop(bus);

  let mut rx = sub.rx;
  let e1 = rx.recv().await.unwrap();
  assert!(matches!(e1, ReporterEvent::WorkerStarted { worker_id: 0 }));
  let e2 = rx.recv().await.unwrap();
  assert!(matches!(e2, ReporterEvent::WorkerFinished { worker_id: 0 }));
  assert!(rx.recv().await.is_none());
}

#[tokio::test]
async fn event_bus_no_subscribers_does_not_panic() {
  let builder = EventBusBuilder::new();
  let bus = builder.build();

  // Should not panic or error.
  bus
    .emit(ReporterEvent::RunStarted {
      total_tests: 1,
      num_workers: 1,
      metadata: serde_json::Value::Null,
    })
    .await;
  drop(bus);
}

#[tokio::test]
async fn event_bus_dropped_subscriber_does_not_block() {
  let mut builder = EventBusBuilder::new();
  let sub1 = builder.subscribe();
  let sub2 = builder.subscribe();
  let bus = builder.build();

  // Drop sub1's receiver — bus should still deliver to sub2 without error.
  drop(sub1);

  bus
    .emit(ReporterEvent::RunStarted {
      total_tests: 1,
      num_workers: 1,
      metadata: serde_json::Value::Null,
    })
    .await;
  drop(bus);

  let mut rx = sub2.rx;
  let event = rx.recv().await.unwrap();
  assert!(matches!(event, ReporterEvent::RunStarted { .. }));
}

// ── ReporterDriver tests ──

/// A test reporter that collects events for verification.
struct CollectorReporter {
  events: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ferridriver_test::reporter::Reporter for CollectorReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    let tag = match event {
      ReporterEvent::RunStarted { .. } => "RunStarted",
      ReporterEvent::RunFinished { .. } => "RunFinished",
      ReporterEvent::TestStarted { .. } => "TestStarted",
      ReporterEvent::TestFinished { .. } => "TestFinished",
      ReporterEvent::WorkerStarted { .. } => "WorkerStarted",
      ReporterEvent::WorkerFinished { .. } => "WorkerFinished",
      ReporterEvent::StepStarted(_) => "StepStarted",
      ReporterEvent::StepFinished(_) => "StepFinished",
    };
    self.events.lock().await.push(tag.to_string());
  }

  async fn finalize(&mut self) -> Result<(), String> {
    self.events.lock().await.push("Finalized".to_string());
    Ok(())
  }
}

#[tokio::test]
async fn reporter_driver_forwards_events_and_finalizes() {
  let collected = Arc::new(Mutex::new(Vec::<String>::new()));
  let reporter = CollectorReporter {
    events: Arc::clone(&collected),
  };
  let reporters = ReporterSet::new(vec![Box::new(reporter)]);

  let mut builder = EventBusBuilder::new();
  let sub = builder.subscribe();
  let bus = builder.build();

  let driver = ReporterDriver::new(reporters, sub);
  let driver_handle = tokio::spawn(driver.run());

  // Emit events.
  bus
    .emit(ReporterEvent::RunStarted {
      total_tests: 2,
      num_workers: 1,
      metadata: serde_json::Value::Null,
    })
    .await;
  bus
    .emit(ReporterEvent::RunFinished {
      total: 2,
      passed: 2,
      failed: 0,
      skipped: 0,
      flaky: 0,
      duration: Duration::from_millis(100),
    })
    .await;

  // Drop bus — closes channel, driver finalizes and exits.
  drop(bus);
  let _ = driver_handle.await;

  let events = collected.lock().await;
  assert_eq!(&*events, &["RunStarted", "RunFinished", "Finalized"]);
}

#[tokio::test]
async fn reporter_driver_returns_reporters_after_run() {
  let collected = Arc::new(Mutex::new(Vec::<String>::new()));
  let reporter = CollectorReporter {
    events: Arc::clone(&collected),
  };
  let reporters = ReporterSet::new(vec![Box::new(reporter)]);

  let mut builder = EventBusBuilder::new();
  let sub = builder.subscribe();
  let bus = builder.build();

  let driver = ReporterDriver::new(reporters, sub);
  let driver_handle = tokio::spawn(driver.run());

  bus
    .emit(ReporterEvent::RunStarted {
      total_tests: 1,
      num_workers: 1,
      metadata: serde_json::Value::Null,
    })
    .await;
  drop(bus);

  // Driver should return the ReporterSet (not consume it permanently).
  let returned_reporters = driver_handle.await.unwrap();
  // ReporterSet should still have reporters (not empty).
  // We verify by checking the collected events include Finalized.
  let events = collected.lock().await;
  assert!(events.contains(&"Finalized".to_string()));
  drop(returned_reporters);
}

#[tokio::test]
async fn real_time_delivery_not_batched() {
  // Verify events arrive at the subscriber as they're emitted,
  // not buffered until the bus is dropped.
  let mut builder = EventBusBuilder::new();
  let sub = builder.subscribe();
  let bus = builder.build();

  let mut rx = sub.rx;

  // Emit one event and immediately check it arrived.
  bus
    .emit(ReporterEvent::RunStarted {
      total_tests: 1,
      num_workers: 1,
      metadata: serde_json::Value::Null,
    })
    .await;

  // Use try_recv — if the event is delivered in real-time, it's already in the channel.
  let result = rx.try_recv();
  assert!(
    result.is_ok(),
    "event should be available immediately (real-time delivery), got: {result:?}"
  );
  assert!(matches!(result.unwrap(), ReporterEvent::RunStarted { .. }));

  // Emit another and verify.
  bus.emit(ReporterEvent::WorkerStarted { worker_id: 0 }).await;
  let result = rx.try_recv();
  assert!(result.is_ok(), "second event should be available immediately");

  drop(bus);
}

#[tokio::test]
async fn concurrent_execution_and_observation() {
  // Simulates the core pattern: execute() emits events while a consumer
  // processes them concurrently via tokio::join!.
  let received = Arc::new(Mutex::new(Vec::<String>::new()));
  let received_clone = Arc::clone(&received);

  let mut builder = EventBusBuilder::new();
  let sub = builder.subscribe();
  let bus = builder.build();

  // Simulate execute() — emits events with small yields between them.
  // Bus is explicitly dropped to close the channel, mirroring how
  // execute(plan, bus) consumes bus by value in real usage.
  let execute_fut = async {
    bus
      .emit(ReporterEvent::RunStarted {
        total_tests: 3,
        num_workers: 1,
      metadata: serde_json::Value::Null,
      })
      .await;
    tokio::task::yield_now().await;
    bus.emit(ReporterEvent::WorkerStarted { worker_id: 0 }).await;
    tokio::task::yield_now().await;
    bus.emit(ReporterEvent::WorkerFinished { worker_id: 0 }).await;
    tokio::task::yield_now().await;
    bus
      .emit(ReporterEvent::RunFinished {
        total: 3,
        passed: 3,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration: Duration::from_millis(50),
      })
      .await;
    drop(bus); // Must explicitly drop — tokio::join! holds MaybeDone alive
  };

  // Simulate TUI drain — receives events as they arrive.
  let drain_fut = async {
    let mut rx = sub.rx;
    while let Some(event) = rx.recv().await {
      let tag = match &event {
        ReporterEvent::RunStarted { .. } => "RunStarted",
        ReporterEvent::RunFinished { .. } => "RunFinished",
        ReporterEvent::WorkerStarted { .. } => "WorkerStarted",
        ReporterEvent::WorkerFinished { .. } => "WorkerFinished",
        _ => "Other",
      };
      received_clone.lock().await.push(tag.to_string());
    }
  };

  tokio::join!(execute_fut, drain_fut);

  let events = received.lock().await;
  assert_eq!(
    &*events,
    &["RunStarted", "WorkerStarted", "WorkerFinished", "RunFinished"]
  );
}
