//! Speaking Playwright's reporter protocol.
//!
//! The embedded UI is Playwright's own front-end, and it learns about a
//! run the way Playwright's reporters do: a stream of JSON events
//! (`teleReceiver.ts::JsonEvent`) that rebuild the test tree and its
//! results on the other side. This module is the translation from our
//! runner's model into those events — nothing here decides anything, it
//! only renames.
//!
//! Two shapes of the same stream:
//!
//! * a **listing** — `onConfigure`, one `onProject` carrying the whole
//!   discovered tree, `onBegin`, `onEnd` — answers `listTests`;
//! * a **run** — the same preamble scoped to the tests being run, then
//!   `onTestBegin` / `onStepBegin` / `onStepEnd` / `onTestEnd` as they
//!   happen, and `onEnd` at the finish.
//!
//! Paths are relative to `rootDir` and re-absolutized in the browser
//! (`teleReceiver.ts::_absolutePath`), so everything here sends them the
//! way the receiver expects rather than as absolute paths.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use crate::config::TestConfig;
use crate::model::{StepLocation, TestAnnotation, TestId, TestOutcome, TestPlan, TestStatus};

/// Result id for one attempt of a test — the receiver keys a result by
/// it, so retries must not collide.
#[must_use]
pub fn result_id(test_id: &str, attempt: u32) -> String {
  format!("{test_id}-attempt{attempt}")
}

/// One project of the run: the name its tests are identified by, and the
/// config it runs under (a project's own `timeout`, `retries`, output and
/// snapshot directories are the merged ones, not the file's top level).
pub struct ProjectInfo<'a> {
  pub name: &'a str,
  pub config: &'a TestConfig,
}

/// `onConfigure`: the run's shape, and the root every relative path in
/// the stream is resolved against.
#[must_use]
pub fn configure(config: &TestConfig, root_dir: &Path, projects: &[ProjectInfo<'_>]) -> Value {
  let summaries: Vec<Value> = projects
    .iter()
    .map(|project| project_summary(root_dir, project))
    .collect();
  json!({
    "method": "onConfigure",
    "params": {
      "config": {
        "globalTimeout": config.global_timeout,
        "maxFailures": config.max_failures,
        "metadata": json!({}),
        "rootDir": root_dir.display().to_string(),
        "version": env!("CARGO_PKG_VERSION"),
        "workers": config.workers,
        "projects": summaries,
      },
    },
  })
}

/// The project entry the config carries, and the one `onProject` sends.
fn project_summary(root_dir: &Path, project: &ProjectInfo<'_>) -> Value {
  let config = project.config;
  json!({
    "name": project.name,
    "grep": [],
    "grepInvert": [],
    "metadata": json!({}),
    "dependencies": [],
    "snapshotDir": relative(root_dir, config.snapshot_dir.as_deref().unwrap_or("__snapshots__")),
    "outputDir": relative(root_dir, &config.output_dir.display().to_string()),
    "repeatEach": config.repeat_each,
    "retries": config.retries,
    "testDir": relative(root_dir, config.test_dir.as_deref().unwrap_or(".")),
    "testIgnore": [],
    "testMatch": [],
    "timeout": config.timeout,
    "use": json!({}),
    "suites": [],
  })
}

/// `onProject`: one project's discovered tree, as file suites holding
/// test cases.
#[must_use]
pub fn project(root_dir: &Path, project: &ProjectInfo<'_>, plan: &TestPlan) -> Value {
  let mut summary = project_summary(root_dir, project);
  summary["suites"] = Value::Array(file_suites(root_dir, project.name, plan));
  json!({ "method": "onProject", "params": { "project": summary } })
}

/// The project's suites: one per file, with describe blocks nested
/// inside — the shape `_mergeSuiteInto` expects (a project's children
/// are files, a file's children are describes).
///
/// Built from each test's own title path rather than from the plan's
/// suite list, because a describe arrives there as a separate suite and
/// the UI wants it as a child of its file.
fn file_suites(root_dir: &Path, project_name: &str, plan: &TestPlan) -> Vec<Value> {
  let mut files: Vec<SuiteNode> = Vec::new();
  for suite in &plan.suites {
    for test in &suite.tests {
      let titles = test.id.title_path();
      let file = relative(root_dir, &suite.file);
      let node = node_for(&mut files, &file, &file);
      // titles = [file, ...describes, test]; the file is the node above,
      // the test is the leaf.
      let describes = &titles[1..titles.len().saturating_sub(1)];
      let mut target = node;
      for describe in describes {
        target = node_for(&mut target.children, describe, &file);
      }
      target
        .tests
        .push(test_case(root_dir, project_name, &test.id, &test.annotations));
    }
  }
  files.into_iter().map(SuiteNode::into_json).collect()
}

/// A suite while it is being built: children first, then its own tests,
/// so the UI lists describes above loose tests as Playwright does.
struct SuiteNode {
  title: String,
  file: String,
  children: Vec<SuiteNode>,
  tests: Vec<Value>,
}

impl SuiteNode {
  fn into_json(self) -> Value {
    let mut entries: Vec<Value> = self.children.into_iter().map(SuiteNode::into_json).collect();
    entries.extend(self.tests);
    json!({
      "title": self.title,
      "location": { "file": self.file, "line": 0, "column": 0 },
      "entries": entries,
    })
  }
}

/// The node named `title` among `nodes`, appended when it is new.
fn node_for<'a>(nodes: &'a mut Vec<SuiteNode>, title: &str, file: &str) -> &'a mut SuiteNode {
  if let Some(index) = nodes.iter().position(|node| node.title == title) {
    return &mut nodes[index];
  }
  nodes.push(SuiteNode {
    title: title.to_string(),
    file: file.to_string(),
    children: Vec::new(),
    tests: Vec::new(),
  });
  nodes.last_mut().unwrap_or_else(|| unreachable!("just pushed"))
}

fn test_case(root_dir: &Path, project_name: &str, id: &TestId, annotations: &[TestAnnotation]) -> Value {
  let titles = id.title_path();
  json!({
    "testId": id.stable_id(project_name),
    // The receiver builds the tree from the suites it is given, so the
    // case's own title is only its last segment.
    "title": titles.last().cloned().unwrap_or_default(),
    "location": {
      "file": relative(root_dir, &id.file),
      "line": id.line.unwrap_or(0),
      "column": 0,
    },
    "retries": 0,
    "tags": tags(annotations),
    "repeatEachIndex": 0,
    "annotations": annotation_values(annotations),
  })
}

fn tags(annotations: &[TestAnnotation]) -> Vec<String> {
  annotations
    .iter()
    .filter_map(|annotation| match annotation {
      TestAnnotation::Tag(tag) => Some(if tag.starts_with('@') {
        tag.clone()
      } else {
        format!("@{tag}")
      }),
      _ => None,
    })
    .collect()
}

fn annotation_values(annotations: &[TestAnnotation]) -> Vec<Value> {
  annotations
    .iter()
    .filter_map(|annotation| match annotation {
      TestAnnotation::Skip { reason, .. } => Some(json!({ "type": "skip", "description": reason })),
      TestAnnotation::Fixme { reason, .. } => Some(json!({ "type": "fixme", "description": reason })),
      TestAnnotation::Fail { reason, .. } => Some(json!({ "type": "fail", "description": reason })),
      TestAnnotation::Slow { reason, .. } => Some(json!({ "type": "slow", "description": reason })),
      TestAnnotation::Info { type_name, description } => Some(json!({ "type": type_name, "description": description })),
      TestAnnotation::Tag(_) | TestAnnotation::Only => None,
    })
    .collect()
}

/// `onBegin`: everything above has been sent, build the tree.
#[must_use]
pub fn begin() -> Value {
  json!({ "method": "onBegin", "params": {} })
}

/// `onEnd`: the run's verdict.
#[must_use]
pub fn end(status: &str, start_wall_ms: f64, duration: Duration) -> Value {
  json!({
    "method": "onEnd",
    "params": {
      "result": {
        "status": status,
        "startTime": start_wall_ms,
        "duration": duration.as_millis() as u64,
      },
    },
  })
}

/// `onTestBegin`: an attempt started.
#[must_use]
pub fn test_begin(test_id: &str, attempt: u32, worker_index: u32, start_wall_ms: f64) -> Value {
  json!({
    "method": "onTestBegin",
    "params": {
      "testId": test_id,
      "result": {
        "id": result_id(test_id, attempt),
        "retry": attempt.saturating_sub(1),
        "workerIndex": worker_index,
        "parallelIndex": worker_index,
        "startTime": start_wall_ms,
      },
    },
  })
}

/// `onTestEnd`: an attempt finished, with its status and errors.
#[must_use]
pub fn test_end(test_id: &str, outcome: &TestOutcome, timeout: Duration) -> Value {
  let expected = if outcome
    .annotations
    .iter()
    .any(|a| matches!(a, TestAnnotation::Skip { .. } | TestAnnotation::Fixme { .. }))
  {
    "skipped"
  } else if outcome.expected_status == crate::model::ExpectedStatus::Fail {
    // `test.fail()`: the attempt really did fail, and only this field
    // tells the UI that failing was the point.
    "failed"
  } else {
    "passed"
  };
  json!({
    "method": "onTestEnd",
    "params": {
      "test": {
        "testId": test_id,
        "expectedStatus": expected,
        "timeout": timeout.as_millis() as u64,
        "annotations": [],
      },
      "result": {
        "id": result_id(test_id, outcome.attempt),
        "duration": outcome.duration.as_millis() as u64,
        "status": status_name(&outcome.status),
        "errors": errors(outcome),
        "annotations": annotation_values(&outcome.annotations),
      },
    },
  })
}

/// Our statuses in Playwright's vocabulary. `flaky` is not a result
/// status there — it is a test that failed once and passed on a retry,
/// which the receiver works out from the results it has.
fn status_name(status: &TestStatus) -> &'static str {
  match status {
    TestStatus::Passed | TestStatus::Flaky => "passed",
    TestStatus::Failed => "failed",
    TestStatus::TimedOut => "timedOut",
    TestStatus::Skipped => "skipped",
    TestStatus::Interrupted => "interrupted",
  }
}

fn errors(outcome: &TestOutcome) -> Vec<Value> {
  outcome
    .error
    .iter()
    .map(|failure| {
      let mut error = json!({ "message": failure.message });
      if let Some(stack) = &failure.stack {
        error["stack"] = Value::String(stack.clone());
      }
      error
    })
    .collect()
}

/// `onStepBegin`: a step of the running attempt.
///
/// `location` is the step's own file — Playwright's
/// `JsonTestStepStart.location` (`isomorphic/teleReceiver.ts:107-114`) —
/// which for a BDD step or an explicit `test.step(…, { location })` is
/// not the spec's.
#[must_use]
pub fn step_begin(
  test_id: &str,
  attempt: u32,
  step_id: &str,
  parent_step_id: Option<&str>,
  title: &str,
  category: &str,
  start_wall_ms: f64,
  location: Option<&StepLocation>,
) -> Value {
  let mut step = json!({
    "id": step_id,
    "parentStepId": parent_step_id,
    "title": title,
    "category": category,
    "startTime": start_wall_ms,
  });
  if let Some(location) = location {
    step["location"] = step_location(location);
  }
  json!({
    "method": "onStepBegin",
    "params": {
      "testId": test_id,
      "resultId": result_id(test_id, attempt),
      "step": step,
    },
  })
}

/// A `Location` as the viewer reads it.
fn step_location(location: &StepLocation) -> Value {
  json!({
    "file": location.file,
    "line": location.line,
    "column": location.column,
  })
}

/// `onStepEnd`: how long the step took, whether it failed, and what it
/// annotated itself with (`stepInfo.skip()`).
#[must_use]
pub fn step_end(
  test_id: &str,
  attempt: u32,
  step_id: &str,
  duration: Duration,
  error: Option<&str>,
  annotations: &[TestAnnotation],
) -> Value {
  let mut step = json!({
    "id": step_id,
    "duration": duration.as_millis() as u64,
  });
  if let Some(error) = error {
    step["error"] = json!({ "message": error });
  }
  if !annotations.is_empty() {
    step["annotations"] = Value::Array(annotation_values(annotations));
  }
  json!({
    "method": "onStepEnd",
    "params": {
      "testId": test_id,
      "resultId": result_id(test_id, attempt),
      "step": step,
    },
  })
}

/// `onAttach`: files a finished attempt produced (the trace above all —
/// the UI opens a finished test's trace from its `trace` attachment).
#[must_use]
pub fn attach(test_id: &str, outcome: &TestOutcome) -> Value {
  let attachments: Vec<Value> = outcome
    .attachments
    .iter()
    .map(|attachment| {
      let mut value = json!({
        "name": attachment.name,
        "contentType": attachment.content_type,
      });
      match &attachment.body {
        crate::model::AttachmentBody::Path(path) => {
          value["path"] = Value::String(path.display().to_string());
        },
        crate::model::AttachmentBody::Bytes(bytes) => {
          use base64::Engine as _;
          value["base64"] = Value::String(base64::engine::general_purpose::STANDARD.encode(bytes));
        },
      }
      value
    })
    .collect();
  json!({
    "method": "onAttach",
    "params": {
      "testId": test_id,
      "resultId": result_id(test_id, outcome.attempt),
      "attachments": attachments,
    },
  })
}

/// `onError`: a failure that belongs to the run rather than to a test
/// (a discovery error, a crashed worker).
#[must_use]
pub fn error(message: &str) -> Value {
  json!({ "method": "onError", "params": { "error": { "message": message } } })
}

/// `onStdIO`: output produced while a test ran.
#[must_use]
pub fn stdio(kind: &str, test_id: Option<&str>, attempt: u32, text: &str) -> Value {
  json!({
    "method": "onStdIO",
    "params": {
      "type": kind,
      "testId": test_id,
      "resultId": test_id.map(|id| result_id(id, attempt)),
      "data": text,
      "isBase64": false,
    },
  })
}

/// Make `path` relative to `root_dir` — the receiver re-absolutizes it
/// against the root it was told about.
fn relative(root_dir: &Path, path: &str) -> String {
  let path = Path::new(path);
  let relative = path.strip_prefix(root_dir).unwrap_or(path);
  relative.display().to_string()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{TestId, TestStep};
  use std::path::PathBuf;

  fn config() -> TestConfig {
    TestConfig {
      output_dir: PathBuf::from("/repo/test-results"),
      test_dir: Some("/repo/tests".to_string()),
      ..TestConfig::default()
    }
  }

  fn test_id() -> TestId {
    TestId {
      file: "/repo/tests/checkout.spec.ts".into(),
      suite: Some("/repo/tests/checkout.spec.ts::checkout".into()),
      name: "pays".into(),
      line: Some(12),
      column: None,
    }
  }

  fn outcome(status: TestStatus) -> TestOutcome {
    TestOutcome {
      test_id: test_id(),
      status,
      duration: Duration::from_millis(1500),
      attempt: 1,
      max_attempts: 1,
      error: None,
      steps: Vec::<TestStep>::new(),
      ..Default::default()
    }
  }

  #[test]
  fn configure_carries_root_and_project() {
    let config = config();
    let event = configure(
      &config,
      Path::new("/repo"),
      &[ProjectInfo {
        name: "cdp-pipe",
        config: &config,
      }],
    );
    assert_eq!(event["method"], "onConfigure");
    assert_eq!(event["params"]["config"]["rootDir"], "/repo");
    assert_eq!(event["params"]["config"]["projects"][0]["name"], "cdp-pipe");
    assert_eq!(
      event["params"]["config"]["projects"][0]["outputDir"], "test-results",
      "paths are relative to rootDir"
    );
  }

  #[test]
  fn configure_carries_every_project_with_its_own_settings() {
    let config = config();
    let mut webkit = config.clone();
    webkit.timeout = 45_000;
    webkit.output_dir = PathBuf::from("/repo/test-results/webkit");
    let event = configure(
      &config,
      Path::new("/repo"),
      &[
        ProjectInfo {
          name: "cdp-pipe",
          config: &config,
        },
        ProjectInfo {
          name: "webkit",
          config: &webkit,
        },
      ],
    );
    let projects = event["params"]["config"]["projects"].as_array().expect("projects");
    assert_eq!(projects.len(), 2, "one entry per project: {projects:#?}");
    assert_eq!(projects[1]["name"], "webkit");
    assert_eq!(
      projects[1]["timeout"], 45_000,
      "a project reports the timeout it merged, not the file's"
    );
    assert_eq!(projects[1]["outputDir"], "test-results/webkit");
  }

  #[test]
  fn a_test_belongs_to_the_project_that_runs_it() {
    let id = test_id();
    assert_ne!(
      id.stable_id("cdp-pipe"),
      id.stable_id("webkit"),
      "the same test in two projects is two ids"
    );
  }

  #[test]
  fn project_groups_tests_under_one_suite_per_file() {
    let mut plan = TestPlan {
      suites: Vec::new(),
      total_tests: 0,
      shard: None,
    };
    let mut suite = crate::model::TestSuite {
      name: "checkout".into(),
      file: "/repo/tests/checkout.spec.ts".into(),
      tests: Vec::new(),
      hooks: crate::model::Hooks::default(),
      annotations: Vec::new(),
      mode: crate::model::SuiteMode::Parallel,
    };
    suite.tests.push(crate::model::TestCase {
      metadata: None,
      id: test_id(),
      test_fn: std::sync::Arc::new(|_| Box::pin(async { Ok(()) })),
      fixture_requests: Vec::new(),
      annotations: vec![TestAnnotation::Tag("smoke".into())],
      timeout: None,
      retries: None,
      expected_status: crate::model::ExpectedStatus::Pass,
      use_options: None,
    });
    plan.total_tests = 1;
    plan.suites.push(suite);

    let config = config();
    let event = project(
      Path::new("/repo"),
      &ProjectInfo {
        name: "cdp-pipe",
        config: &config,
      },
      &plan,
    );
    let suites = event["params"]["project"]["suites"].as_array().expect("suites");
    assert_eq!(suites.len(), 1);
    assert_eq!(suites[0]["title"], "tests/checkout.spec.ts");
    // The describe block is a suite of its own, under the file.
    let describe = &suites[0]["entries"][0];
    assert_eq!(describe["title"], "checkout");
    let case = &describe["entries"][0];
    assert_eq!(case["title"], "pays");
    assert_eq!(case["location"]["file"], "tests/checkout.spec.ts");
    assert_eq!(case["location"]["line"], 12);
    assert_eq!(case["tags"][0], "@smoke");
    assert_eq!(
      case["testId"].as_str().map(str::len),
      Some(41),
      "ids are Playwright-shaped: two 20-char hashes and a dash"
    );
  }

  #[test]
  fn a_result_id_is_stable_per_attempt() {
    assert_eq!(result_id("abc-def", 1), "abc-def-attempt1");
    assert_ne!(result_id("abc-def", 1), result_id("abc-def", 2));
  }

  #[test]
  fn statuses_translate_to_playwright_vocabulary() {
    assert_eq!(status_name(&TestStatus::Passed), "passed");
    assert_eq!(status_name(&TestStatus::Flaky), "passed");
    assert_eq!(status_name(&TestStatus::TimedOut), "timedOut");
    assert_eq!(status_name(&TestStatus::Interrupted), "interrupted");
  }

  #[test]
  fn test_end_reports_errors_and_expected_status() {
    let mut failed = outcome(TestStatus::Failed);
    failed.error = Some(crate::model::TestFailure {
      message: "expect(received).toBe(expected)".into(),
      stack: Some("at spec.ts:3".into()),
      diff: None,
      screenshot: None,
    });
    let event = test_end("t1", &failed, Duration::from_secs(30));
    assert_eq!(event["params"]["result"]["status"], "failed");
    assert_eq!(
      event["params"]["result"]["errors"][0]["message"],
      "expect(received).toBe(expected)"
    );
    assert_eq!(event["params"]["test"]["expectedStatus"], "passed");
    assert_eq!(event["params"]["test"]["timeout"], 30_000);

    let mut skipped = outcome(TestStatus::Skipped);
    skipped.annotations.push(TestAnnotation::Skip {
      reason: Some("wip".into()),
      condition: None,
    });
    let event = test_end("t1", &skipped, Duration::from_secs(30));
    assert_eq!(event["params"]["test"]["expectedStatus"], "skipped");
    assert_eq!(event["params"]["result"]["annotations"][0]["type"], "skip");
  }

  #[test]
  fn steps_carry_their_result_and_parent() {
    let begin = step_begin(
      "t1",
      1,
      "s2",
      Some("s1"),
      "click",
      "test.step",
      10.0,
      Some(&StepLocation::new("features/checkout.feature", 12)),
    );
    assert_eq!(begin["params"]["resultId"], "t1-attempt1");
    assert_eq!(begin["params"]["step"]["parentStepId"], "s1");
    // A step may name a file the spec does not, and the viewer reads it
    // from here.
    assert_eq!(begin["params"]["step"]["location"]["file"], "features/checkout.feature");
    assert_eq!(begin["params"]["step"]["location"]["line"], 12);
    let annotations = [TestAnnotation::Info {
      type_name: "skip".into(),
      description: "not on webkit".into(),
    }];
    let end = step_end("t1", 1, "s2", Duration::from_millis(88), Some("boom"), &annotations);
    assert_eq!(end["params"]["step"]["duration"], 88);
    assert_eq!(end["params"]["step"]["error"]["message"], "boom");
    assert_eq!(end["params"]["step"]["annotations"][0]["type"], "skip");
    assert_eq!(end["params"]["step"]["annotations"][0]["description"], "not on webkit");
  }

  #[test]
  fn a_step_without_a_location_omits_the_key() {
    let begin = step_begin("t1", 1, "s1", None, "click", "test.step", 10.0, None);
    assert!(begin["params"]["step"].get("location").is_none());
  }

  #[test]
  fn attachments_travel_by_path_or_inline() {
    let mut with_trace = outcome(TestStatus::Passed);
    with_trace.attachments.push(crate::model::Attachment {
      name: "trace".into(),
      content_type: "application/zip".into(),
      body: crate::model::AttachmentBody::Path(PathBuf::from("/repo/test-results/t/trace.zip")),
      step_id: None,
    });
    with_trace.attachments.push(crate::model::Attachment {
      name: "note".into(),
      content_type: "text/plain".into(),
      body: crate::model::AttachmentBody::Bytes(b"hi".to_vec()),
      step_id: None,
    });
    let event = attach("t1", &with_trace);
    let attachments = event["params"]["attachments"].as_array().expect("attachments");
    assert_eq!(attachments[0]["path"], "/repo/test-results/t/trace.zip");
    assert_eq!(attachments[1]["base64"], "aGk=");
  }
}
