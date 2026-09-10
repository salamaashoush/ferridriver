use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use ferridriver_test::reporter::{
  EventBus, EventBusBuilder, Reporter, ReporterDriver, ReporterEvent, ReporterSet, RunStatus, Subscription,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum Event {
  RunStarted {
    total_tests: usize,
    num_workers: u32,
  },
  RunFinished {
    total: usize,
    passed: usize,
    failed: usize,
    skipped: usize,
    duration_ms: u64,
  },
  WorkerStarted {
    worker_id: u32,
  },
  WorkerFinished {
    worker_id: u32,
  },
}

impl From<Event> for ReporterEvent {
  fn from(event: Event) -> Self {
    match event {
      Event::RunStarted {
        total_tests,
        num_workers,
      } => Self::RunStarted {
        total_tests,
        num_workers,
        metadata: Value::Null,
        start_time: SystemTime::now(),
        preamble: Arc::new(ferridriver_test::reporter::api::RunPreamble::empty()),
      },
      Event::RunFinished {
        total,
        passed,
        failed,
        skipped,
        duration_ms,
      } => Self::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky: 0,
        duration: Duration::from_millis(duration_ms),
        status: RunStatus::Passed,
      },
      Event::WorkerStarted { worker_id } => Self::WorkerStarted { worker_id },
      Event::WorkerFinished { worker_id } => Self::WorkerFinished { worker_id },
    }
  }
}

pub(super) fn observation(event: &ReporterEvent) -> Value {
  match event {
    ReporterEvent::RunStarted {
      total_tests,
      num_workers,
      ..
    } => json!({ "kind": "RunStarted", "totalTests": total_tests, "numWorkers": num_workers }),
    ReporterEvent::RunFinished {
      total,
      passed,
      failed,
      skipped,
      duration,
      ..
    } => json!({ "kind": "RunFinished", "total": total, "passed": passed, "failed": failed,
        "skipped": skipped, "durationMs": duration.as_millis() }),
    ReporterEvent::WorkerStarted { worker_id } => json!({ "kind": "WorkerStarted", "workerId": worker_id }),
    ReporterEvent::WorkerFinished { worker_id } => json!({ "kind": "WorkerFinished", "workerId": worker_id }),
    ReporterEvent::TestStarted { test_id, .. } => json!({ "kind": "TestStarted", "name": test_id.name }),
    ReporterEvent::TestFinished { outcome } => json!({ "kind": "TestFinished", "name": outcome.test_id.name }),
    other => json!({ "unexpected": format!("{other:?}") }),
  }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
  Emit {
    sender: usize,
    event: Event,
  },
  Clone {
    sender: usize,
  },
  DropSender {
    sender: usize,
  },
  DropSubscriber {
    subscriber: usize,
  },
  Receive {
    subscriber: usize,
    #[serde(default)]
    immediate: bool,
  },
  Drain {
    subscriber: usize,
  },
  Join {
    subscriber: usize,
  },
  Yield,
}

struct Bus {
  senders: Vec<Option<EventBus>>,
  subscribers: Vec<Option<Subscription>>,
  drains: Vec<Option<JoinHandle<Vec<Value>>>>,
}

impl Bus {
  fn sender(&self, index: usize) -> Result<&EventBus> {
    self
      .senders
      .get(index)
      .and_then(Option::as_ref)
      .context("missing sender")
  }

  fn subscriber(&mut self, index: usize) -> Result<&mut Option<Subscription>> {
    self.subscribers.get_mut(index).context("missing subscriber")
  }

  async fn run(&mut self, action: Action) -> Result<Value> {
    match action {
      Action::Emit { sender, event } => self.sender(sender)?.emit(event.into()),
      Action::Clone { sender } => self.senders.push(Some(self.sender(sender)?.clone())),
      Action::DropSender { sender } => {
        self.senders.get_mut(sender).context("missing sender")?.take();
      },
      Action::DropSubscriber { subscriber } => {
        self.subscriber(subscriber)?.take();
      },
      Action::Receive { subscriber, immediate } => {
        let rx = &mut self.subscriber(subscriber)?.as_mut().context("dropped subscriber")?.rx;
        return Ok(if immediate {
          match rx.try_recv() {
            Ok(event) => observation(&event),
            Err(error) => json!({ "error": error.to_string() }),
          }
        } else {
          rx.recv().await.as_ref().map_or(Value::Null, observation)
        });
      },
      Action::Drain { subscriber } => {
        let mut rx = self.subscriber(subscriber)?.take().context("dropped subscriber")?.rx;
        self.drains[subscriber] = Some(tokio::spawn(async move {
          let mut events = Vec::new();
          while let Some(event) = rx.recv().await {
            events.push(observation(&event));
          }
          events
        }));
      },
      Action::Join { subscriber } => {
        let task = self
          .drains
          .get_mut(subscriber)
          .and_then(Option::take)
          .context("missing drain")?;
        return Ok(json!(task.await?));
      },
      Action::Yield => tokio::task::yield_now().await,
    }
    Ok(Value::Null)
  }
}

pub async fn bus(subscribers: usize, actions: Vec<Action>) -> Result<Value> {
  let mut builder = EventBusBuilder::new();
  let subscribers: Vec<_> = (0..subscribers).map(|_| Some(builder.subscribe())).collect();
  let mut bus = Bus {
    drains: (0..subscribers.len()).map(|_| None).collect(),
    subscribers,
    senders: vec![Some(builder.build())],
  };
  let mut observations = Vec::new();
  for action in actions {
    observations.push(bus.run(action).await?);
  }
  Ok(json!(observations))
}

pub(super) struct Collector(pub(super) Arc<Mutex<Vec<Value>>>);

#[async_trait::async_trait]
impl Reporter for Collector {
  async fn on_event(&mut self, event: &ReporterEvent) {
    self.0.lock().await.push(observation(event));
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    self.0.lock().await.push(json!({ "kind": "Finalized" }));
    Ok(())
  }
}

pub async fn driver(events: Vec<Event>) -> Result<Value> {
  let collected = Arc::new(Mutex::new(Vec::new()));
  let reporters = ReporterSet::new(vec![Box::new(Collector(Arc::clone(&collected)))]);
  let mut builder = EventBusBuilder::new();
  let subscription = builder.subscribe();
  let bus = builder.build();
  let task = tokio::spawn(ReporterDriver::new(reporters, subscription).run());
  for event in events {
    bus.emit(event.into());
  }
  drop(bus);
  let reporters = task.await?;
  Ok(json!({ "events": *collected.lock().await, "retained": !reporters.is_empty() }))
}
