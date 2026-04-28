//! Bridge from Playwright's TS `Reporter` interface into ferridriver's
//! event bus. The user-facing ergonomics live in TS via
//! `defineReporter(impl)`, which collapses the per-method object into
//! a single dispatcher function (eventName, args). The dispatcher is
//! registered through the NAPI call below; this module wraps it as a
//! Rust [`Reporter`] that translates each `ReporterEvent` variant into
//! the matching JS payload shape.
//!
//! The translation aims to mirror the shapes that
//! `/tmp/playwright/packages/playwright/types/test.d.ts::TestCase`,
//! `TestResult`, `TestStep`, `FullConfig`, `Suite`, `FullResult`
//! describe. Where ferridriver doesn't yet emit a particular field
//! (e.g. step `errors[]` chains, suite hierarchy), the corresponding
//! JS field is omitted rather than stubbed with a placeholder.

use std::sync::Arc;

use napi::bindgen_prelude::Function;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use serde_json::{Value, json};

/// TSFN signature: `(eventName: string, args: any[]) => unknown`. The
/// dispatcher returns whatever the user's `Reporter` method returned,
/// which for Playwright is either `void` or — for `onEnd` — a Promise
/// that may resolve with a status patch. We don't await the result
/// here because all reporter callbacks are fire-and-forget on the
/// Rust side.
pub type DispatcherFn =
  ThreadsafeFunction<Value, napi::bindgen_prelude::Unknown<'static>, Value, napi::Status, false, true, 0>;

/// Wrap a TS dispatcher Function into a Rust Reporter implementation.
pub struct JsReporter {
  dispatcher: Arc<DispatcherFn>,
  /// `true` after the run completes — guards against `onExit` calls
  /// piggy-backing on a stale dispatcher when the runner is reused.
  exited: bool,
}

impl JsReporter {
  /// Build a JsReporter from a JS dispatcher function. The TS-side
  /// helper [`packages/ferridriver-test/src/reporter.ts::defineReporter`]
  /// builds this dispatcher from the user's [`Reporter`] object by
  /// switching on `eventName`.
  pub fn build(callback: Function<'_, Value, napi::bindgen_prelude::Unknown<'static>>) -> napi::Result<Self> {
    let tsfn = callback
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    Ok(Self {
      dispatcher: Arc::new(tsfn),
      exited: false,
    })
  }

  fn dispatch(&self, event: &str, args: Vec<Value>) {
    let payload = json!({
      "event": event,
      "args": args,
    });
    self.dispatcher.call(payload, ThreadsafeFunctionCallMode::NonBlocking);
  }
}

#[async_trait::async_trait]
impl ferridriver_test::reporter::Reporter for JsReporter {
  async fn on_event(&mut self, event: &ferridriver_test::reporter::ReporterEvent) {
    use ferridriver_test::reporter::ReporterEvent;
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        metadata,
      } => {
        let config = json!({
          "metadata": metadata,
          "workers": num_workers,
        });
        let suite = json!({
          "title": "",
          "totalTests": total_tests,
          "tests": [],
          "suites": [],
        });
        self.dispatch("onBegin", vec![config, suite]);
      },
      ReporterEvent::WorkerStarted { worker_id } => {
        self.dispatch("onWorkerStarted", vec![json!({ "workerId": worker_id })]);
      },
      ReporterEvent::TestStarted { test_id, attempt } => {
        let test = test_case_from_id(test_id);
        let result = json!({
          "retry": attempt.saturating_sub(1),
          "workerIndex": 0,
          "parallelIndex": 0,
          "status": "running",
          "stdout": [],
          "stderr": [],
          "errors": [],
          "attachments": [],
          "steps": [],
        });
        self.dispatch("onTestBegin", vec![test, result]);
      },
      ReporterEvent::StepStarted(ev) => {
        let test = test_case_from_id(&ev.test_id);
        let result = json!({
          "retry": 0,
          "status": "running",
        });
        let step = json!({
          "title": ev.title,
          "category": format!("{:?}", ev.category).to_lowercase(),
          "stepId": ev.step_id,
          "parentStepId": ev.parent_step_id,
        });
        self.dispatch("onStepBegin", vec![test, result, step]);
      },
      ReporterEvent::StepFinished(ev) => {
        let test = test_case_from_id(&ev.test_id);
        let result = json!({
          "retry": 0,
          "status": if ev.error.is_some() { "failed" } else { "passed" },
        });
        let step = json!({
          "title": ev.title,
          "category": format!("{:?}", ev.category).to_lowercase(),
          "stepId": ev.step_id,
          "duration": ev.duration.as_secs_f64() * 1000.0,
          "error": ev.error.as_ref().map(|m| json!({ "message": m })),
          "metadata": ev.metadata,
        });
        self.dispatch("onStepEnd", vec![test, result, step]);
      },
      ReporterEvent::TestFinished { test_id, outcome } => {
        let test = test_case_from_id(test_id);
        let result = test_result_from_outcome(outcome);
        self.dispatch("onTestEnd", vec![test, result]);
      },
      ReporterEvent::WorkerFinished { worker_id } => {
        self.dispatch("onWorkerFinished", vec![json!({ "workerId": worker_id })]);
      },
      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => {
        let status = if *failed > 0 { "failed" } else { "passed" };
        let result = json!({
          "status": status,
          "startTime": null,
          "duration": duration.as_secs_f64() * 1000.0,
          "totals": {
            "total": total,
            "passed": passed,
            "failed": failed,
            "skipped": skipped,
            "flaky": flaky,
          },
        });
        self.dispatch("onEnd", vec![result]);
      },
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    if !self.exited {
      self.exited = true;
      self.dispatch("onExit", vec![]);
    }
    Ok(())
  }
}

fn test_case_from_id(test_id: &ferridriver_test::model::TestId) -> Value {
  let mut path: Vec<String> = Vec::with_capacity(2);
  if let Some(ref suite) = test_id.suite {
    if !suite.is_empty() {
      path.push(suite.clone());
    }
  }
  path.push(test_id.name.clone());
  json!({
    "id": test_id.full_name(),
    "title": test_id.name,
    "location": {
      "file": test_id.file,
      "line": test_id.line,
      "column": 0,
    },
    "titlePath": path,
  })
}

fn test_result_from_outcome(outcome: &ferridriver_test::model::TestOutcome) -> Value {
  let status = match outcome.status {
    ferridriver_test::model::TestStatus::Passed => "passed",
    ferridriver_test::model::TestStatus::Failed => "failed",
    ferridriver_test::model::TestStatus::TimedOut => "timedOut",
    ferridriver_test::model::TestStatus::Skipped => "skipped",
    ferridriver_test::model::TestStatus::Flaky => "flaky",
    ferridriver_test::model::TestStatus::Interrupted => "interrupted",
  };
  let errors = outcome
    .error
    .as_ref()
    .map(|e| {
      json!([{
        "message": e.message,
        "stack": e.stack,
      }])
    })
    .unwrap_or_else(|| json!([]));
  let attachments: Vec<Value> = outcome
    .attachments
    .iter()
    .map(|a| {
      let path = match &a.body {
        ferridriver_test::model::AttachmentBody::Path(p) => Some(p.display().to_string()),
        ferridriver_test::model::AttachmentBody::Bytes(_) => None,
      };
      json!({
        "name": a.name,
        "contentType": a.content_type,
        "path": path,
      })
    })
    .collect();
  json!({
    "status": status,
    "duration": outcome.duration.as_secs_f64() * 1000.0,
    "retry": outcome.attempt.saturating_sub(1),
    "stdout": [outcome.stdout.clone()],
    "stderr": [outcome.stderr.clone()],
    "errors": errors,
    "error": outcome.error.as_ref().map(|e| json!({ "message": e.message, "stack": e.stack })),
    "attachments": attachments,
    "steps": [],
    "workerIndex": 0,
    "parallelIndex": 0,
  })
}
