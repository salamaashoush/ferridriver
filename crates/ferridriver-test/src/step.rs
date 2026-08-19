//! What a `test.step` IS: its options, where it says it happened, what
//! its body is allowed to do, and how it ends.
//!
//! The rules live here rather than in a host because both JS hosts and
//! the Rust test API open steps, and a step opened from `#[ferritest]`
//! must time out, box its error and resolve its location exactly as one
//! opened from a spec does. A host only marshals: it parses its own
//! option bag into [`StepOptions`], hands [`run`] a future for the body,
//! and re-raises whatever comes back.
//!
//! Divergence from Playwright, deliberate and load-bearing: a step
//! timeout here races the parked clock ([`ferridriver::pause::run_within`]),
//! so time spent stopped at `--debug` does not count against it.
//! Playwright races a wall-clock `raceAgainstDeadline` outside its
//! `TimeoutManager` (`common/testType.ts:286-298`), so upstream a paused
//! debugger does NOT suspend a step timeout. Documented in
//! `docs/playwright-compat.md`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::model::{StepCategory, StepLocation, StepStatus, TestAnnotation};

/// Playwright's `test.step(title, body, { box, location, timeout })`.
#[derive(Debug, Clone, Default)]
pub struct StepOptions {
  /// Attribute an error inside the step to the step's own call site
  /// rather than to the line that raised it.
  pub box_step: bool,
  /// An explicit location, which may name a file the spec does not —
  /// the `.feature` line a BDD step came from, a generated source.
  pub location: Option<StepLocation>,
  /// Fail the step (not the test) when the body outlives this.
  pub timeout: Option<Duration>,
}

/// `test.step` vs `test.step.skip`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StepExpectation {
  #[default]
  Pass,
  /// The body never runs and the step records a `skip` annotation.
  Skip,
}

/// One frame of the stack a step was opened from.
///
/// A host that executes bundled code reports its own coordinates and the
/// step driver maps them back through the host's source map; a caller
/// that already knows the authored position passes it directly.
#[derive(Debug, Clone)]
pub enum StepFrame {
  Host { line: u32, column: u32 },
  Source(StepLocation),
}

/// A step opening, as its caller describes it.
#[derive(Debug, Clone)]
pub struct StepSpec {
  pub title: String,
  pub category: StepCategory,
  /// Recorded onto [`crate::model::TestStep::metadata`]. A fixture's
  /// `{ box: true }` carries its grouping here, which is what keeps a
  /// framework's own fixtures out of the way of a test's steps without
  /// hiding them.
  pub metadata: Option<serde_json::Value>,
  pub options: StepOptions,
  pub expectation: StepExpectation,
  /// Call-site frames, innermost first. `box` re-attributes the step to
  /// the second one, which is why more than the innermost is carried.
  pub frames: Vec<StepFrame>,
}

impl StepSpec {
  #[must_use]
  pub fn new(title: impl Into<String>) -> Self {
    Self {
      title: title.into(),
      category: StepCategory::TestStep,
      metadata: None,
      options: StepOptions::default(),
      expectation: StepExpectation::Pass,
      frames: Vec::new(),
    }
  }

  #[must_use]
  pub fn with_options(mut self, options: StepOptions) -> Self {
    self.options = options;
    self
  }

  #[must_use]
  pub fn with_frames(mut self, frames: Vec<StepFrame>) -> Self {
    self.frames = frames;
    self
  }

  #[must_use]
  pub fn expecting(mut self, expectation: StepExpectation) -> Self {
    self.expectation = expectation;
    self
  }
}

/// What the runner answers with when a step opens.
#[derive(Debug, Clone, Default)]
pub struct StepStarted {
  pub step_id: String,
  /// Where the step says it happened, after [`resolve_location`].
  pub location: Option<StepLocation>,
  /// Frames a boxed step's error is re-attributed to. Empty unless the
  /// step (or an enclosing one) asked to be boxed.
  pub boxed_stack: Vec<StepLocation>,
  /// `[...test.titlePath, ...enclosing step titles, title]`.
  pub title_path: Vec<String>,
}

/// How a step ended.
#[derive(Debug, Clone)]
pub struct StepOutcome {
  pub status: StepStatus,
  pub error: Option<String>,
  pub annotations: Vec<TestAnnotation>,
}

/// A step body that raised. Host-neutral: a JS exception, a Rust error.
#[derive(Debug, Clone)]
pub struct StepBodyError {
  pub name: String,
  pub message: String,
  /// Replaced with [`boxed_error_stack`] when the step is boxed.
  pub stack: Option<String>,
}

impl StepBodyError {
  #[must_use]
  pub fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      message: message.into(),
      stack: None,
    }
  }
}

/// How a step failed, as its caller sees it.
#[derive(Debug, Clone)]
pub enum StepError {
  /// The body raised. `stack` is already re-attributed for a boxed step.
  Body(StepBodyError),
  /// The body outlived `{ timeout }`.
  Timeout { message: String },
}

impl StepError {
  #[must_use]
  pub fn message(&self) -> &str {
    match self {
      Self::Body(e) => &e.message,
      Self::Timeout { message } => message,
    }
  }

  #[must_use]
  pub fn name(&self) -> &str {
    match self {
      Self::Body(e) => &e.name,
      Self::Timeout { .. } => "TimeoutError",
    }
  }

  #[must_use]
  pub fn stack(&self) -> Option<&str> {
    match self {
      Self::Body(e) => e.stack.as_deref(),
      Self::Timeout { .. } => None,
    }
  }
}

impl std::fmt::Display for StepError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.message())
  }
}

/// Playwright's `TestStepInfo`, minus the language it is written in.
///
/// Handed to the body so it can annotate itself, skip itself, or name
/// its own place in the title tree. A host mirrors it onto whatever
/// object its callers expect.
pub struct StepRun {
  started: StepStarted,
  expectation: StepExpectation,
  annotations: Mutex<Vec<TestAnnotation>>,
  skipped: AtomicBool,
}

impl StepRun {
  fn new(started: StepStarted, expectation: StepExpectation) -> Self {
    Self {
      started,
      expectation,
      annotations: Mutex::new(Vec::new()),
      skipped: AtomicBool::new(false),
    }
  }

  #[must_use]
  pub fn started(&self) -> &StepStarted {
    &self.started
  }

  #[must_use]
  pub fn step_id(&self) -> &str {
    &self.started.step_id
  }

  #[must_use]
  pub fn title_path(&self) -> &[String] {
    &self.started.title_path
  }

  /// Playwright's `_runStepBody(skip, …)`: a `test.step.skip` never runs
  /// its body at all.
  #[must_use]
  pub fn should_run_body(&self) -> bool {
    self.expectation != StepExpectation::Skip
  }

  /// `step.skip()` / `test.step.skip(...)` — record the annotation and
  /// mark the step skipped. The body is expected to unwind afterwards
  /// (a host raises its own sentinel and swallows it at the boundary).
  pub fn record_skip(&self, description: Option<String>) {
    self.skipped.store(true, Ordering::Relaxed);
    self.annotate("skip", description);
  }

  /// A free-form annotation, as `step.annotations.push(...)` makes.
  pub fn annotate(&self, type_name: impl Into<String>, description: Option<String>) {
    self
      .annotations
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(TestAnnotation::Info {
        type_name: type_name.into(),
        description: description.unwrap_or_default(),
      });
  }

  #[must_use]
  pub fn annotations(&self) -> Vec<TestAnnotation> {
    self
      .annotations
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  #[must_use]
  pub fn was_skipped(&self) -> bool {
    self.skipped.load(Ordering::Relaxed)
  }
}

/// Future type of the [`StepDriver`] methods.
pub type StepFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Somewhere a step can be opened and closed.
///
/// Implemented by `TestInfo` (the Rust test API) and by the host bridge
/// (which resolves host coordinates first, then delegates to the same
/// `TestInfo`), so both hosts and Rust tests produce the same reporter
/// events from the same rules.
pub trait StepDriver: Send + Sync {
  fn begin_step(&self, spec: StepSpec) -> StepFuture<'_, StepStarted>;
  fn end_step(&self, step_id: String, outcome: StepOutcome) -> StepFuture<'_, ()>;
}

/// Playwright's step-location rule, verbatim
/// (`worker/testInfo.ts:298-304`):
///
/// - a boxed step inherits an enclosing boxed step's frames rather than
///   taking its own, so only the outermost box re-attributes;
/// - otherwise `box` takes every frame ABOVE the call site, and the
///   step's location becomes the first of them — the line that called
///   the function containing the step;
/// - an explicit `location` always wins;
/// - failing all of that, the step is at its own call site.
#[must_use]
pub fn resolve_location(
  options: &StepOptions,
  frames: &[StepLocation],
  parent_boxed: &[StepLocation],
) -> (Option<StepLocation>, Vec<StepLocation>) {
  let boxed = if !parent_boxed.is_empty() {
    parent_boxed.to_vec()
  } else if options.box_step {
    frames.iter().skip(1).cloned().collect()
  } else {
    Vec::new()
  };
  let location = options
    .location
    .clone()
    .or_else(|| options.box_step.then(|| boxed.first().cloned()).flatten())
    .or_else(|| frames.first().cloned());
  (location, boxed)
}

/// Playwright's `stringifyStackFrames` (`utils/stackTrace.ts:123-132`)
/// for frames that name no function.
#[must_use]
pub fn stringify_frames(frames: &[StepLocation]) -> String {
  frames
    .iter()
    .map(|f| format!("    at {}:{}:{}", f.file, f.line, f.column))
    .collect::<Vec<_>>()
    .join("\n")
}

/// The stack a boxed step's error carries instead of its own
/// (`worker/testInfo.ts:328-329`): the message, then the frames outside
/// the box.
#[must_use]
pub fn boxed_error_stack(message: &str, frames: &[StepLocation]) -> String {
  format!("{message}\n{}", stringify_frames(frames))
}

/// Playwright's step-timeout message (`common/testType.ts:298`).
#[must_use]
pub fn timeout_message(timeout: Duration) -> String {
  format!("Step timeout of {}ms exceeded.", timeout.as_millis())
}

/// Open a step, run its body under the step's rules, close it.
///
/// The body gets the [`StepRun`] Playwright passes as `TestStepInfo`. It
/// must consult [`StepRun::should_run_body`] before calling user code,
/// and report a body that skipped itself through
/// [`StepRun::record_skip`].
///
/// # Errors
///
/// [`StepError::Timeout`] when the body outlives `{ timeout }`,
/// [`StepError::Body`] when it raised — with the stack already
/// re-attributed if the step is boxed.
pub async fn run<T, F>(
  driver: &dyn StepDriver,
  spec: StepSpec,
  body: impl FnOnce(Arc<StepRun>) -> F,
) -> Result<T, StepError>
where
  F: Future<Output = Result<T, StepBodyError>>,
{
  let expectation = spec.expectation;
  let timeout = spec.options.timeout;
  let started = driver.begin_step(spec).await;
  let step_id = started.step_id.clone();
  let boxed = started.boxed_stack.clone();
  let run = Arc::new(StepRun::new(started, expectation));
  if expectation == StepExpectation::Skip {
    run.record_skip(None);
  }

  let body = body(Arc::clone(&run));
  let result: Result<T, StepError> = match timeout {
    Some(limit) => match ferridriver::pause::run_within(limit, body).await {
      Ok(inner) => inner.map_err(StepError::Body),
      Err(_) => Err(StepError::Timeout {
        message: timeout_message(limit),
      }),
    },
    None => body.await.map_err(StepError::Body),
  };

  let result = result.map_err(|error| match error {
    StepError::Body(mut e) if !boxed.is_empty() => {
      e.stack = Some(boxed_error_stack(&e.message, &boxed));
      StepError::Body(e)
    },
    other => other,
  });

  let status = match (&result, run.was_skipped()) {
    (Err(_), _) => StepStatus::Failed,
    (Ok(_), true) => StepStatus::Skipped,
    (Ok(_), false) => StepStatus::Passed,
  };
  driver
    .end_step(
      step_id,
      StepOutcome {
        status,
        error: result.as_ref().err().map(|e| e.message().to_string()),
        annotations: run.annotations(),
      },
    )
    .await;
  result
}

#[cfg(test)]
mod tests {
  use super::{StepLocation, StepOptions, boxed_error_stack, resolve_location, stringify_frames, timeout_message};

  fn at(file: &str, line: u32) -> StepLocation {
    StepLocation {
      file: file.to_string(),
      line,
      column: 3,
    }
  }

  #[test]
  fn an_unboxed_step_is_at_its_own_call_site() {
    let frames = vec![at("spec.ts", 9), at("spec.ts", 15)];
    let (location, boxed) = resolve_location(&StepOptions::default(), &frames, &[]);
    assert_eq!(location.map(|l| l.line), Some(9));
    assert!(boxed.is_empty());
  }

  #[test]
  fn a_boxed_step_is_at_the_line_that_called_it() {
    let frames = vec![at("helpers.ts", 9), at("spec.ts", 15), at("spec.ts", 40)];
    let options = StepOptions {
      box_step: true,
      ..StepOptions::default()
    };
    let (location, boxed) = resolve_location(&options, &frames, &[]);
    assert_eq!(location.map(|l| l.line), Some(15));
    assert_eq!(boxed.iter().map(|f| f.line).collect::<Vec<_>>(), vec![15, 40]);
  }

  #[test]
  fn a_nested_box_inherits_the_outer_one() {
    let outer = vec![at("spec.ts", 15)];
    let frames = vec![at("helpers.ts", 22), at("helpers.ts", 30)];
    let options = StepOptions {
      box_step: true,
      ..StepOptions::default()
    };
    let (location, boxed) = resolve_location(&options, &frames, &outer);
    assert_eq!(location.map(|l| l.line), Some(15));
    assert_eq!(boxed.len(), 1);
  }

  #[test]
  fn an_explicit_location_beats_every_frame() {
    let frames = vec![at("spec.ts", 9), at("spec.ts", 15)];
    let options = StepOptions {
      box_step: true,
      location: Some(at("features/checkout.feature", 12)),
      ..StepOptions::default()
    };
    let (location, boxed) = resolve_location(&options, &frames, &[]);
    let location = location.expect("explicit location");
    assert_eq!(
      (location.file.as_str(), location.line),
      ("features/checkout.feature", 12)
    );
    // Still boxed: the error attribution and the reported location are
    // separate decisions.
    assert!(!boxed.is_empty());
  }

  #[test]
  fn a_step_with_no_frames_and_no_option_has_no_location() {
    let (location, boxed) = resolve_location(&StepOptions::default(), &[], &[]);
    assert!(location.is_none());
    assert!(boxed.is_empty());
  }

  #[test]
  fn frames_stringify_the_way_playwright_prints_them() {
    assert_eq!(stringify_frames(&[at("spec.ts", 15)]), "    at spec.ts:15:3");
    assert_eq!(
      boxed_error_stack("boom", &[at("spec.ts", 15), at("spec.ts", 40)]),
      "boom\n    at spec.ts:15:3\n    at spec.ts:40:3"
    );
  }

  #[test]
  fn the_timeout_message_is_playwrights() {
    assert_eq!(
      timeout_message(std::time::Duration::from_millis(200)),
      "Step timeout of 200ms exceeded."
    );
  }

  // ── The orchestration ────────────────────────────────────────────

  use std::sync::{Arc, Mutex};
  use std::time::Duration;

  use super::{
    StepBodyError, StepDriver, StepError, StepExpectation, StepFuture, StepOutcome, StepSpec, StepStarted, run,
  };
  use crate::model::StepStatus;

  #[derive(Default)]
  struct Recorder {
    ended: Mutex<Vec<StepOutcome>>,
  }

  impl StepDriver for Recorder {
    fn begin_step(&self, spec: StepSpec) -> StepFuture<'_, StepStarted> {
      let frames: Vec<StepLocation> = spec
        .frames
        .iter()
        .filter_map(|f| match f {
          super::StepFrame::Source(loc) => Some(loc.clone()),
          super::StepFrame::Host { .. } => None,
        })
        .collect();
      let (location, boxed_stack) = resolve_location(&spec.options, &frames, &[]);
      Box::pin(async move {
        StepStarted {
          step_id: "s1".to_string(),
          location,
          boxed_stack,
          title_path: vec![spec.title],
        }
      })
    }

    fn end_step(&self, _step_id: String, outcome: StepOutcome) -> StepFuture<'_, ()> {
      self
        .ended
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(outcome);
      Box::pin(async {})
    }
  }

  impl Recorder {
    fn last(&self) -> StepOutcome {
      self
        .ended
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .last()
        .cloned()
        .expect("a step ended")
    }
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn a_body_that_outlives_its_timeout_fails_the_step_alone() {
    let driver = Recorder::default();
    let spec = StepSpec::new("slow").with_options(StepOptions {
      timeout: Some(Duration::from_millis(50)),
      ..StepOptions::default()
    });
    let result: Result<(), StepError> = run(&driver, spec, |_| async {
      std::future::pending::<()>().await;
      Ok(())
    })
    .await;
    match result {
      Err(StepError::Timeout { message }) => assert_eq!(message, "Step timeout of 50ms exceeded."),
      other => panic!("expected a step timeout, got {other:?}"),
    }
    assert_eq!(driver.last().status, StepStatus::Failed);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn a_skip_expectation_never_runs_the_body_and_reports_skipped() {
    let driver = Recorder::default();
    let ran = Arc::new(Mutex::new(false));
    let seen = Arc::clone(&ran);
    let result: Result<u8, StepError> = run(
      &driver,
      StepSpec::new("unsupported").expecting(StepExpectation::Skip),
      move |run| async move {
        if run.should_run_body() {
          *seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        }
        Ok(0)
      },
    )
    .await;
    assert!(result.is_ok());
    assert!(!*ran.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    let outcome = driver.last();
    assert_eq!(outcome.status, StepStatus::Skipped);
    assert_eq!(outcome.annotations.len(), 1);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn a_boxed_failure_carries_the_callers_stack() {
    let driver = Recorder::default();
    let spec = StepSpec::new("login")
      .with_options(StepOptions {
        box_step: true,
        ..StepOptions::default()
      })
      .with_frames(vec![
        super::StepFrame::Source(at("helpers.ts", 9)),
        super::StepFrame::Source(at("spec.ts", 15)),
      ]);
    let result: Result<(), StepError> = run(&driver, spec, |_| async {
      Err(StepBodyError::new("Error", "rejected"))
    })
    .await;
    let StepError::Body(error) = result.expect_err("the body failed") else {
      panic!("expected a body failure");
    };
    assert_eq!(error.stack.as_deref(), Some("rejected\n    at spec.ts:15:3"));
    assert_eq!(driver.last().status, StepStatus::Failed);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn a_body_that_skips_itself_reports_skipped_without_failing() {
    let driver = Recorder::default();
    let result: Result<(), StepError> = run(&driver, StepSpec::new("maybe"), |run| async move {
      run.record_skip(Some("not here".to_string()));
      Ok(())
    })
    .await;
    assert!(result.is_ok());
    assert_eq!(driver.last().status, StepStatus::Skipped);
  }
}
