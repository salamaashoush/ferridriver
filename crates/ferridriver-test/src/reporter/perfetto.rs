//! Perfetto reporter: the run as a Chrome trace-event document that
//! `ui.perfetto.dev` and `chrome://tracing` open as a timeline.
//!
//! A port of Playwright's `reporters/perfetto.ts`. Every attempt is a
//! slice on the lane of the worker slot that ran it, its steps are nested
//! slices clamped inside it, and an error that belongs to no test is a
//! global instant. Timestamps are microseconds from the earliest event.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rustc_hash::FxHashMap;
use serde::Serialize;

use crate::model::{AttachmentBody, ExpectedStatus, TestAnnotation, TestOutcome, TestStatus, TestStep};
use crate::reporter::base::{self, ResultCollector, TestRecord};
use crate::reporter::{Reporter, ReporterEvent};

const PROCESS_ID: u32 = 1;
const RUN_THREAD_ID: u32 = 0;

#[derive(Serialize)]
struct TraceEvent {
  name: String,
  cat: String,
  ph: &'static str,
  ts: i64,
  #[serde(skip_serializing_if = "Option::is_none")]
  dur: Option<i64>,
  pid: u32,
  tid: u32,
  #[serde(skip_serializing_if = "Option::is_none")]
  s: Option<&'static str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  cname: Option<&'static str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  args: Option<serde_json::Value>,
}

impl TraceEvent {
  fn metadata(name: &str, tid: u32, args: serde_json::Value) -> Self {
    Self {
      name: name.into(),
      cat: "__metadata".into(),
      ph: "M",
      ts: 0,
      dur: None,
      pid: PROCESS_ID,
      tid,
      s: None,
      cname: None,
      args: Some(args),
    }
  }
}

pub struct PerfettoReporter {
  output_path: PathBuf,
  root_dir: PathBuf,
  collector: ResultCollector,
  /// Errors outside any test, with the moment each was reported.
  global_errors: Vec<(i64, String)>,
}

impl PerfettoReporter {
  #[must_use]
  pub fn new(output_path: PathBuf) -> Self {
    Self {
      output_path,
      root_dir: PathBuf::from("."),
      collector: ResultCollector::new(),
      global_errors: Vec::new(),
    }
  }

  /// The directory locations and attachment paths are written relative to.
  #[must_use]
  pub fn with_root_dir(mut self, root_dir: impl Into<PathBuf>) -> Self {
    self.root_dir = root_dir.into();
    self
  }

  fn relative_path(&self, file: &str) -> String {
    let path = Path::new(file);
    let relative = path.strip_prefix(&self.root_dir).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
  }

  fn location(&self, file: &str, line: usize, column: usize) -> String {
    format!("{}:{line}:{column}", self.relative_path(file))
  }

  fn attachment(&self, attachment: &crate::model::Attachment) -> String {
    match &attachment.body {
      AttachmentBody::Path(path) => self.relative_path(&path.to_string_lossy()),
      AttachmentBody::Bytes(_) => attachment.name.clone(),
    }
  }

  fn test_args(&self, record: &TestRecord, attempt: &TestOutcome) -> serde_json::Value {
    let id = &attempt.test_id;
    let mut args = serde_json::Map::new();
    args.insert("status".into(), attempt.status.as_str().into());
    args.insert(
      "expectedStatus".into(),
      match attempt.expected_status {
        ExpectedStatus::Pass => "passed",
        ExpectedStatus::Fail => "failed",
      }
      .into(),
    );
    args.insert("testId".into(), record.stable_id().into());
    args.insert("workerIndex".into(), attempt.worker_index.into());
    args.insert("parallelIndex".into(), attempt.parallel_index.into());
    args.insert("timeout".into(), millis(attempt.timeout).into());
    if let Some(line) = id.line {
      args.insert(
        "location".into(),
        self.location(&id.file, line, id.column.unwrap_or(0)).into(),
      );
    }
    // The file leads the title path; what follows is the describes and
    // the test's own title.
    let titles = id.title_path();
    if titles.len() > 2 {
      args.insert("title".into(), titles[1..].join(" \u{203a} ").into());
    }
    if !attempt.project_name.is_empty() {
      args.insert("project".into(), attempt.project_name.clone().into());
    }
    if attempt.attempt > 1 {
      args.insert("retry".into(), (attempt.attempt - 1).into());
    }
    let tags = crate::reporter::api::tags(&attempt.annotations);
    if !tags.is_empty() {
      args.insert("tags".into(), tags.join(" ").into());
    }
    let annotations = annotations(&attempt.annotations);
    if !annotations.is_empty() {
      args.insert("annotations".into(), annotations.into());
    }
    if !attempt.attachments.is_empty() {
      let attachments: Vec<String> = attempt.attachments.iter().map(|a| self.attachment(a)).collect();
      args.insert("attachments".into(), attachments.into());
    }
    let errors: Vec<String> = base::attempt_errors(attempt)
      .into_iter()
      .map(|error| format_error(&error.message, error.stack.as_deref()))
      .collect();
    if !errors.is_empty() {
      args.insert("errors".into(), errors.into());
    }
    if !attempt.stdout.is_empty() {
      args.insert("stdout".into(), attempt.stdout.clone().into());
    }
    if !attempt.stderr.is_empty() {
      args.insert("stderr".into(), attempt.stderr.clone().into());
    }
    serde_json::Value::Object(args)
  }

  fn step_args(&self, step: &TestStep, attempt: &TestOutcome) -> Option<serde_json::Value> {
    let mut args = serde_json::Map::new();
    if let Some(params) = &step.params {
      args.insert("params".into(), params.clone());
    }
    if let Some(location) = &step.location {
      args.insert(
        "location".into(),
        self
          .location(&location.file, location.line as usize, location.column as usize)
          .into(),
      );
    }
    let annotations = annotations(&step.annotations);
    if !annotations.is_empty() {
      args.insert("annotations".into(), annotations.into());
    }
    let attachments: Vec<String> = attempt
      .attachments
      .iter()
      .filter(|a| a.step_id.as_deref() == Some(step.step_id.as_str()))
      .map(|a| self.attachment(a))
      .collect();
    if !attachments.is_empty() {
      args.insert("attachments".into(), attachments.into());
    }
    if let Some(error) = &step.error {
      args.insert("error".into(), base::strip_ansi(error).into_owned().into());
    }
    (!args.is_empty()).then_some(serde_json::Value::Object(args))
  }

  fn events(&self) -> (Vec<TraceEvent>, Vec<u32>) {
    let mut attempts: Vec<(&TestRecord, &TestOutcome)> = self
      .collector
      .records()
      .iter()
      .flat_map(|record| record.attempts.iter().map(move |attempt| (record, &**attempt)))
      .collect();
    attempts.sort_by_key(|(_, attempt)| micros(attempt.start_time));

    let mut events = Vec::new();
    let mut lanes = Lanes::default();
    for (record, attempt) in attempts {
      let steps = StepTree::new(&attempt.steps);
      let start = micros(attempt.start_time);
      let mut end = start + duration_micros(attempt.duration);
      for step in &steps.roots {
        end = end.max(steps.end_time(step));
      }
      let lane = lanes.allocate(attempt.parallel_index, start, end);
      let tid = lane + 1;
      events.push(TraceEvent {
        name: attempt.test_id.name.clone(),
        cat: "test".into(),
        ph: "X",
        ts: start,
        dur: Some(end - start),
        pid: PROCESS_ID,
        tid,
        s: None,
        cname: status_color(attempt.status),
        args: Some(self.test_args(record, attempt)),
      });
      let mut slices = StepSlices {
        reporter: self,
        events: &mut events,
        steps: &steps,
        attempt,
        tid,
      };
      for step in &steps.roots {
        slices.append(step, start, end);
      }
    }
    for (timestamp, error) in &self.global_errors {
      events.push(TraceEvent {
        name: "error".into(),
        cat: "error".into(),
        ph: "i",
        ts: *timestamp,
        dur: None,
        pid: PROCESS_ID,
        tid: RUN_THREAD_ID,
        s: Some("g"),
        cname: Some("bad"),
        args: Some(serde_json::json!({ "error": error })),
      });
    }
    (events, lanes.used())
  }

  fn document(&self) -> Vec<u8> {
    let (mut events, lanes) = self.events();
    let run_start = micros(self.collector.run.start_time);
    let origin = events.iter().map(|event| event.ts).fold(run_start, i64::min);

    let mut all = vec![
      TraceEvent::metadata(
        "process_name",
        RUN_THREAD_ID,
        serde_json::json!({ "name": "ferridriver test" }),
      ),
      TraceEvent::metadata(
        "process_sort_index",
        RUN_THREAD_ID,
        serde_json::json!({ "sort_index": 0 }),
      ),
    ];
    for lane in lanes {
      let tid = lane + 1;
      all.push(TraceEvent::metadata(
        "thread_name",
        tid,
        serde_json::json!({ "name": format!("Worker {lane}") }),
      ));
      all.push(TraceEvent::metadata(
        "thread_sort_index",
        tid,
        serde_json::json!({ "sort_index": tid }),
      ));
    }
    // Stable, so a parent slice stays ahead of its children on a tie.
    events.sort_by_key(|event| event.ts);
    for event in &mut events {
      event.ts -= origin;
    }
    all.append(&mut events);

    let document = serde_json::json!({
      "traceEvents": all,
      "displayTimeUnit": "ms",
      "metadata": {
        "ferridriver-version": env!("CARGO_PKG_VERSION"),
        "start-time": self.collector.run.start_iso8601(),
        "duration": millis(self.collector.run.duration),
        "status": self.collector.run.status.as_str(),
      },
    });
    serde_json::to_vec(&document).unwrap_or_default()
  }
}

/// Writes the step slices of one attempt onto its lane.
struct StepSlices<'a> {
  reporter: &'a PerfettoReporter,
  events: &'a mut Vec<TraceEvent>,
  steps: &'a StepTree<'a>,
  attempt: &'a TestOutcome,
  tid: u32,
}

impl StepSlices<'_> {
  /// A step is drawn inside its parent whatever its own clock said.
  fn append(&mut self, step: &TestStep, parent_start: i64, parent_end: i64) {
    let step_start = micros(step.start_time);
    let start = step_start.clamp(parent_start, parent_end);
    let end = (step_start + duration_micros(step.duration)).clamp(start, parent_end);
    self.events.push(TraceEvent {
      name: match &step.subtitle {
        Some(subtitle) => format!("{} {subtitle}", step.title),
        None => step.title.clone(),
      },
      cat: step.category.to_string(),
      ph: "X",
      ts: start,
      dur: Some(end - start),
      pid: PROCESS_ID,
      tid: self.tid,
      s: None,
      cname: step.error.is_some().then_some("bad"),
      args: self.reporter.step_args(step, self.attempt),
    });
    for child in self.steps.children(step) {
      self.append(child, start, end);
    }
  }
}

/// Which lane each attempt is drawn on. An attempt takes its worker
/// slot's lane unless something still running occupies it, which a
/// merged report of several shards can produce; then the first free one.
#[derive(Default)]
struct Lanes {
  end_time: FxHashMap<u32, i64>,
}

impl Lanes {
  fn allocate(&mut self, preferred: u32, start: i64, end: i64) -> u32 {
    let busy = |lanes: &Self, lane: u32| lanes.end_time.get(&lane).is_some_and(|&lane_end| lane_end > start);
    let lane = if busy(self, preferred) {
      // One more lane than are in use: at least one of them is free.
      let lanes = u32::try_from(self.end_time.len()).unwrap_or(u32::MAX);
      (0..=lanes).find(|&lane| !busy(self, lane)).unwrap_or(preferred)
    } else {
      preferred
    };
    self.end_time.insert(lane, end);
    lane
  }

  fn used(&self) -> Vec<u32> {
    let mut lanes: Vec<u32> = self.end_time.keys().copied().collect();
    lanes.sort_unstable();
    lanes
  }
}

/// An attempt's steps as a tree. The runner records them flat, each
/// naming its parent, in the order they finished; a merged report nests
/// them. Both read the same here, children in the order they started.
struct StepTree<'a> {
  roots: Vec<&'a TestStep>,
  by_parent: FxHashMap<&'a str, Vec<&'a TestStep>>,
}

impl<'a> StepTree<'a> {
  fn new(steps: &'a [TestStep]) -> Self {
    let known: rustc_hash::FxHashSet<&str> = steps.iter().map(|step| step.step_id.as_str()).collect();
    let mut roots = Vec::new();
    let mut by_parent: FxHashMap<&str, Vec<&TestStep>> = FxHashMap::default();
    for step in steps {
      match step.parent_step_id.as_deref().filter(|parent| known.contains(parent)) {
        Some(parent) => by_parent.entry(parent).or_default().push(step),
        None => roots.push(step),
      }
    }
    roots.sort_by_key(|step| micros(step.start_time));
    for children in by_parent.values_mut() {
      children.sort_by_key(|step| micros(step.start_time));
    }
    Self { roots, by_parent }
  }

  fn children(&self, step: &'a TestStep) -> Vec<&'a TestStep> {
    let mut children: Vec<&TestStep> = step.steps.iter().collect();
    if let Some(flat) = self.by_parent.get(step.step_id.as_str()) {
      children.extend(flat.iter().copied());
    }
    children.sort_by_key(|child| micros(child.start_time));
    children
  }

  fn end_time(&self, step: &'a TestStep) -> i64 {
    let mut end = micros(step.start_time) + duration_micros(step.duration);
    for child in self.children(step) {
      end = end.max(self.end_time(child));
    }
    end
  }
}

/// Palette names `chrome://tracing` understands.
fn status_color(status: TestStatus) -> Option<&'static str> {
  match status {
    TestStatus::Passed => Some("good"),
    TestStatus::Failed => Some("bad"),
    TestStatus::TimedOut => Some("terrible"),
    TestStatus::Interrupted => Some("yellow"),
    TestStatus::Skipped => Some("grey"),
    TestStatus::Flaky => None,
  }
}

/// `type: description`, or the type alone. Tags have their own field and
/// a lock only schedules the test.
fn annotations(annotations: &[TestAnnotation]) -> Vec<String> {
  annotations
    .iter()
    .filter_map(|annotation| {
      let (kind, description) = match annotation {
        TestAnnotation::Tag(_) | TestAnnotation::Lock(_) => return None,
        TestAnnotation::Skip { reason, .. } => ("skip", reason.clone()),
        TestAnnotation::Slow { reason, .. } => ("slow", reason.clone()),
        TestAnnotation::Fixme { reason, .. } => ("fixme", reason.clone()),
        TestAnnotation::Fail { reason, .. } => ("fail", reason.clone()),
        TestAnnotation::Only => ("only", None),
        TestAnnotation::Info { type_name, description } => (type_name.as_str(), Some(description.clone())),
      };
      Some(match description.filter(|text| !text.is_empty()) {
        Some(description) => format!("{kind}: {description}"),
        None => kind.to_string(),
      })
    })
    .collect()
}

fn format_error(message: &str, stack: Option<&str>) -> String {
  let message = base::strip_ansi(message);
  match stack.map(base::strip_ansi) {
    Some(stack) if !stack.is_empty() && !stack.contains(message.as_ref()) => format!("{message}\n{stack}"),
    Some(stack) if !stack.is_empty() => stack.into_owned(),
    _ => message.into_owned(),
  }
}

fn micros(time: SystemTime) -> i64 {
  time
    .duration_since(SystemTime::UNIX_EPOCH)
    .ok()
    .and_then(|since| i64::try_from(since.as_micros()).ok())
    .unwrap_or_default()
}

fn duration_micros(duration: Duration) -> i64 {
  i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

fn millis(duration: Duration) -> u64 {
  u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[async_trait::async_trait]
impl Reporter for PerfettoReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    if let ReporterEvent::RunError { error } = event {
      self.global_errors.push((
        micros(SystemTime::now()),
        format_error(&error.message, error.stack.as_deref()),
      ));
    }
    self.collector.observe(event);
  }

  async fn finalize(&mut self) -> ferridriver::error::Result<()> {
    let document = self.document();
    if let Some(parent) = self.output_path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(&self.output_path)?;
    if self.output_path.extension().is_some_and(|extension| extension == "gz") {
      let mut gzip = flate2::write::GzEncoder::new(file, flate2::Compression::default());
      gzip.write_all(&document)?;
      gzip.finish()?;
    } else {
      let mut file = file;
      file.write_all(&document)?;
    }
    tracing::info!("Perfetto trace written to {}", self.output_path.display());
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use std::io::Read as _;
  use std::sync::Arc;

  use super::*;
  use crate::model::{Attachment, StepCategory, StepLocation, StepStatus, TestFailure, TestId};
  use crate::reporter::RunStatus;

  const T0: u64 = 1_700_000_000_000_000;

  struct ScopedDir(PathBuf);
  impl Drop for ScopedDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn scoped(name: &str) -> ScopedDir {
    let path = std::env::temp_dir().join(format!("ferri-perfetto-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp dir");
    ScopedDir(path)
  }

  fn at(micros: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_micros(T0 + micros)
  }

  fn outcome(name: &str, start: u64, millis: u64, status: TestStatus) -> TestOutcome {
    TestOutcome {
      test_id: TestId {
        repeat_each_index: 0,
        file: "tests/a.test.ts".into(),
        suite: None,
        name: name.into(),
        line: Some(7),
        column: Some(3),
      },
      status,
      duration: Duration::from_millis(millis),
      start_time: at(start),
      timeout: Duration::from_secs(30),
      ..Default::default()
    }
  }

  fn step(id: &str, parent: Option<&str>, title: &str, category: StepCategory, start: u64, millis: u64) -> TestStep {
    TestStep {
      step_id: id.into(),
      title: title.into(),
      category,
      duration: Duration::from_millis(millis),
      subtitle: None,
      params: None,
      start_time: at(start),
      status: StepStatus::Passed,
      error: None,
      location: None,
      annotations: Vec::new(),
      parent_step_id: parent.map(Into::into),
      metadata: None,
      steps: Vec::new(),
    }
  }

  /// The document a run of `outcomes` leaves, read back from `file`.
  async fn trace(file: &str, outcomes: Vec<TestOutcome>, errors: Vec<&str>, status: RunStatus) -> serde_json::Value {
    // One directory per test: they run side by side.
    let test = std::thread::current().name().unwrap_or("perfetto").replace("::", "-");
    let dir = scoped(&test);
    let path = dir.0.join(file);
    let mut reporter = PerfettoReporter::new(path.clone()).with_root_dir("tests");
    reporter.collector.run.start_time = at(0);
    for outcome in outcomes {
      reporter
        .on_event(&ReporterEvent::TestFinished {
          outcome: Arc::new(outcome),
        })
        .await;
    }
    for message in errors {
      reporter
        .on_event(&ReporterEvent::RunError {
          error: Box::new(TestFailure {
            message: message.into(),
            stack: None,
            diff: None,
            screenshot: None,
            error_context: None,
          }),
        })
        .await;
    }
    reporter
      .on_event(&ReporterEvent::RunFinished {
        total: 0,
        passed: 0,
        failed: 0,
        skipped: 0,
        flaky: 0,
        duration: Duration::from_millis(1234),
        status,
      })
      .await;
    reporter.finalize().await.expect("finalize");
    let bytes = std::fs::read(&path).expect("report");
    let gzipped = bytes.starts_with(&[0x1f, 0x8b]);
    assert_eq!(gzipped, path.extension().is_some_and(|extension| extension == "gz"));
    let text = if gzipped {
      let mut text = String::new();
      flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_string(&mut text)
        .expect("gunzip");
      text
    } else {
      String::from_utf8(bytes).expect("utf8")
    };
    serde_json::from_str(&text).expect("json")
  }

  fn events(trace: &serde_json::Value) -> Vec<serde_json::Value> {
    trace["traceEvents"].as_array().cloned().unwrap_or_default()
  }

  fn slices(trace: &serde_json::Value) -> Vec<serde_json::Value> {
    events(trace).into_iter().filter(|event| event["ph"] == "X").collect()
  }

  fn slice(trace: &serde_json::Value, name: &str) -> serde_json::Value {
    slices(trace)
      .into_iter()
      .find(|event| event["name"] == name)
      .unwrap_or_else(|| panic!("no slice named {name}"))
  }

  fn thread_names(trace: &serde_json::Value) -> Vec<String> {
    events(trace)
      .iter()
      .filter(|event| event["ph"] == "M" && event["name"] == "thread_name")
      .filter_map(|event| event["args"]["name"].as_str().map(ToString::to_string))
      .collect()
  }

  fn passing() -> TestOutcome {
    let mut passing = outcome("passing @smoke", 1_000, 40, TestStatus::Passed);
    passing.test_id.suite = Some("tests/a.test.ts::suite".into());
    passing.project_name = "chromium".into();
    passing.stdout = "hello from the test\n".into();
    passing.annotations = vec![
      TestAnnotation::Tag("smoke".into()),
      TestAnnotation::Info {
        type_name: "issue".into(),
        description: "flaky".into(),
      },
    ];
    let mut outer = step("s1", None, "outer", StepCategory::TestStep, 5_000, 20);
    outer.location = Some(StepLocation::new("tests/a.test.ts", 9));
    passing.steps = vec![
      // Finish order, as the runner records them: the child first.
      step("s2", Some("s1"), "inner", StepCategory::TestStep, 6_000, 5),
      outer,
      step("s0", None, "Before Hooks", StepCategory::Hook, 1_000, 2),
      step("s3", Some("s2"), "Expect \"toBe\"", StepCategory::Expect, 7_000, 1),
    ];
    passing
  }

  fn failing() -> TestOutcome {
    let mut failing = outcome("failing", 50_000, 10, TestStatus::Failed);
    failing.errors = vec![TestFailure {
      message: "\u{1b}[31mexpect(received).toBe(expected)\u{1b}[39m".into(),
      stack: Some("    at tests/a.test.ts:14:5".into()),
      diff: None,
      screenshot: None,
      error_context: None,
    }];
    failing
  }

  #[tokio::test]
  async fn writes_tests_and_steps_as_slices() {
    let trace = trace(
      "perfetto.json",
      vec![passing(), failing()],
      Vec::new(),
      RunStatus::Failed,
    )
    .await;
    assert_eq!(trace["displayTimeUnit"], "ms");
    assert_eq!(trace["metadata"]["status"], "failed");
    assert_eq!(trace["metadata"]["duration"], 1234);
    assert_eq!(trace["metadata"]["ferridriver-version"], env!("CARGO_PKG_VERSION"));
    let process = events(&trace)
      .into_iter()
      .find(|event| event["name"] == "process_name")
      .expect("process name");
    assert_eq!(process["args"]["name"], "ferridriver test");
    assert_eq!(thread_names(&trace), ["Worker 0"]);

    let passing = slice(&trace, "passing @smoke");
    assert_eq!(passing["cat"], "test");
    assert_eq!(passing["cname"], "good");
    // Microseconds from the run's start, which is the earliest moment.
    assert_eq!(passing["ts"], 1_000);
    assert_eq!(passing["dur"], 40_000);
    assert_eq!(passing["tid"], 1);
    let args = &passing["args"];
    assert_eq!(args["status"], "passed");
    assert_eq!(args["expectedStatus"], "passed");
    assert_eq!(args["title"], "suite \u{203a} passing @smoke");
    assert_eq!(args["project"], "chromium");
    assert_eq!(args["workerIndex"], 0);
    assert_eq!(args["parallelIndex"], 0);
    assert_eq!(args["timeout"], 30_000);
    assert_eq!(args["tags"], "@smoke");
    assert_eq!(args["annotations"], serde_json::json!(["issue: flaky"]));
    assert_eq!(args["stdout"], "hello from the test\n");
    assert_eq!(args["location"], "a.test.ts:7:3");
    assert!(args.get("retry").is_none());
    assert!(args.get("errors").is_none());

    let failing = slice(&trace, "failing");
    assert_eq!(failing["cname"], "bad");
    assert_eq!(failing["args"]["status"], "failed");
    // A lone test keeps its title to the slice name.
    assert!(failing["args"].get("title").is_none());
    assert_eq!(
      failing["args"]["errors"],
      serde_json::json!(["expect(received).toBe(expected)\n    at tests/a.test.ts:14:5"])
    );

    assert_eq!(slice(&trace, "Before Hooks")["cat"], "hook");
    assert_eq!(slice(&trace, "Expect \"toBe\"")["cat"], "expect");
    let outer = slice(&trace, "outer");
    assert_eq!(outer["cat"], "test.step");
    assert_eq!(outer["args"]["location"], "a.test.ts:9:0");
    assert!(slice(&trace, "inner").get("args").is_none());
  }

  #[tokio::test]
  async fn slices_on_a_lane_form_a_stack() {
    let mut overrun = passing();
    // A step whose clock runs past its parent, and one that starts
    // before the test: both are drawn inside what contains them.
    overrun
      .steps
      .push(step("s4", Some("s1"), "late", StepCategory::PwApi, 20_000, 500));
    overrun
      .steps
      .push(step("s5", None, "early", StepCategory::Fixture, 0, 3));
    let trace = trace("perfetto.json", vec![overrun, failing()], Vec::new(), RunStatus::Failed).await;

    let mut by_lane: FxHashMap<u64, Vec<(i64, i64, String)>> = FxHashMap::default();
    for event in slices(&trace) {
      by_lane.entry(event["tid"].as_u64().expect("tid")).or_default().push((
        event["ts"].as_i64().expect("ts"),
        event["dur"].as_i64().expect("dur"),
        event["name"].as_str().expect("name").to_string(),
      ));
    }
    for list in by_lane.values_mut() {
      list.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
      let mut stack: Vec<&(i64, i64, String)> = Vec::new();
      for event in list.iter() {
        while stack.last().is_some_and(|top| top.0 + top.1 <= event.0) {
          stack.pop();
        }
        if let Some(parent) = stack.last() {
          assert!(
            event.0 + event.1 <= parent.0 + parent.1,
            "{} inside {}",
            event.2,
            parent.2
          );
        }
        stack.push(event);
      }
    }

    let outer = slice(&trace, "outer");
    let inner = slice(&trace, "inner");
    let late = slice(&trace, "late");
    let end = |event: &serde_json::Value| event["ts"].as_i64().expect("ts") + event["dur"].as_i64().expect("dur");
    assert!(inner["ts"].as_i64() >= outer["ts"].as_i64());
    assert!(end(&inner) <= end(&outer));
    assert_eq!(end(&late), end(&outer));
    assert_eq!(late["cat"], "pw:api");
    let test = slice(&trace, "passing @smoke");
    assert_eq!(slice(&trace, "early")["ts"], test["ts"]);
    // The overrunning step stretched the test past its own duration.
    assert_eq!(end(&test), 520_000);
  }

  #[tokio::test]
  async fn a_step_shows_its_subtitle_and_params() {
    let mut buys = outcome("buys", 0, 30, TestStatus::Passed);
    let mut checkout = step("s1", None, "checkout", StepCategory::TestStep, 1_000, 20);
    checkout.subtitle = Some("SKU 42".into());
    checkout.params = Some(serde_json::json!({ "orderId": 42 }));
    buys.steps = vec![checkout, step("s2", None, "plain", StepCategory::TestStep, 22_000, 1)];
    let trace = trace("perfetto.json", vec![buys], Vec::new(), RunStatus::Passed).await;
    let checkout = slice(&trace, "checkout SKU 42");
    assert_eq!(checkout["args"], serde_json::json!({ "params": { "orderId": 42 } }));
    assert!(slice(&trace, "plain").get("args").is_none());
  }

  #[tokio::test]
  async fn nested_steps_read_like_flat_ones() {
    let mut nested = outcome("merged", 0, 30, TestStatus::Passed);
    let mut outer = step("s1", None, "outer", StepCategory::TestStep, 1_000, 20);
    outer.steps = vec![step("s2", Some("s1"), "inner", StepCategory::TestStep, 2_000, 5)];
    nested.steps = vec![outer];
    let trace = trace("perfetto.json", vec![nested], Vec::new(), RunStatus::Passed).await;
    let names: Vec<String> = slices(&trace)
      .iter()
      .filter_map(|event| event["name"].as_str().map(ToString::to_string))
      .collect();
    assert_eq!(names, ["merged", "outer", "inner"]);
    assert_eq!(slice(&trace, "inner")["ts"], 2_000);
  }

  #[tokio::test]
  async fn each_worker_slot_has_a_lane() {
    let one = outcome("one", 0, 500, TestStatus::Passed);
    let mut two = outcome("two", 10_000, 500, TestStatus::Passed);
    two.parallel_index = 1;
    two.worker_index = 1;
    // Same slot while `one` still runs, as two merged shards report it.
    let three = outcome("three", 20_000, 100, TestStatus::Skipped);
    let after = outcome("after", 600_000, 10, TestStatus::TimedOut);
    let trace = trace(
      "perfetto.json",
      vec![three, two, after, one],
      Vec::new(),
      RunStatus::Passed,
    )
    .await;
    assert_eq!(thread_names(&trace), ["Worker 0", "Worker 1", "Worker 2"]);
    assert_eq!(slice(&trace, "one")["tid"], 1);
    assert_eq!(slice(&trace, "two")["tid"], 2);
    assert_eq!(slice(&trace, "three")["tid"], 3);
    assert_eq!(slice(&trace, "after")["tid"], 1);
    assert_eq!(slice(&trace, "three")["cname"], "grey");
    assert_eq!(slice(&trace, "after")["cname"], "terrible");
  }

  #[tokio::test]
  async fn attachments_are_a_name_or_a_relative_path() {
    let mut one = outcome("one", 0, 10, TestStatus::Passed);
    let mut attached = step("s1", None, "attach", StepCategory::TestStep, 1_000, 1);
    attached.error = Some("\u{1b}[31mboom\u{1b}[39m".into());
    one.steps = vec![attached];
    one.attachments = vec![
      Attachment {
        name: "inline".into(),
        content_type: "text/plain".into(),
        body: AttachmentBody::Bytes(b"body".to_vec()),
        step_id: None,
      },
      Attachment {
        name: "file".into(),
        content_type: "text/plain".into(),
        body: AttachmentBody::Path("tests/test-results/a-one/note.txt".into()),
        step_id: Some("s1".into()),
      },
    ];
    let trace = trace("perfetto.json", vec![one], Vec::new(), RunStatus::Passed).await;
    assert_eq!(
      slice(&trace, "one")["args"]["attachments"],
      serde_json::json!(["inline", "test-results/a-one/note.txt"])
    );
    let step = slice(&trace, "attach");
    assert_eq!(
      step["args"]["attachments"],
      serde_json::json!(["test-results/a-one/note.txt"])
    );
    assert_eq!(step["args"]["error"], "boom");
    assert_eq!(step["cname"], "bad");
  }

  #[tokio::test]
  async fn a_gz_output_file_is_gzipped() {
    let one = outcome("one", 0, 10, TestStatus::Passed);
    let trace = trace("reports/perfetto.json.gz", vec![one], Vec::new(), RunStatus::Passed).await;
    assert_eq!(slice(&trace, "one")["args"]["status"], "passed");
  }

  #[tokio::test]
  async fn an_output_path_that_is_a_directory_fails() {
    for name in ["trace.json", "trace.json.gz"] {
      let dir = scoped(&format!("dir-{}", name.replace('.', "-")));
      let path = dir.0.join(name);
      std::fs::create_dir_all(&path).expect("directory");
      let mut reporter = PerfettoReporter::new(path);
      assert!(reporter.finalize().await.is_err(), "{name}");
    }
  }

  #[tokio::test]
  async fn retries_are_separate_slices() {
    let first = outcome("flaky", 0, 10, TestStatus::Failed);
    let mut second = outcome("flaky", 20_000, 10, TestStatus::Passed);
    second.attempt = 2;
    let trace = trace("perfetto.json", vec![first, second], Vec::new(), RunStatus::Passed).await;
    let flaky: Vec<serde_json::Value> = slices(&trace)
      .into_iter()
      .filter(|event| event["name"] == "flaky")
      .collect();
    assert_eq!(flaky.len(), 2);
    assert_eq!(flaky[0]["args"]["status"], "failed");
    assert_eq!(flaky[1]["args"]["status"], "passed");
    assert!(flaky[0]["args"].get("retry").is_none());
    assert_eq!(flaky[1]["args"]["retry"], 1);
  }

  #[tokio::test]
  async fn an_error_outside_any_test_is_a_global_instant() {
    let one = outcome("one", 0, 10, TestStatus::Passed);
    let trace = trace("perfetto.json", vec![one], vec!["Oh my!"], RunStatus::Failed).await;
    let instants: Vec<serde_json::Value> = events(&trace).into_iter().filter(|event| event["ph"] == "i").collect();
    assert_eq!(instants.len(), 1);
    assert_eq!(instants[0]["args"]["error"], "Oh my!");
    assert_eq!(instants[0]["s"], "g");
    assert_eq!(instants[0]["tid"], 0);
    assert_eq!(instants[0]["cname"], "bad");
    assert!(instants[0].get("dur").is_none());
  }
}
