#![allow(clippy::expect_used, clippy::unwrap_used, clippy::large_futures)]
//! JS reporters: the hooks fire in Playwright's order with Playwright's
//! objects, the two reporter interfaces stay apart, a throwing reporter
//! cannot take the run down, and `preprocess` / `onEnd` reach back into
//! the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ferridriver_test::config::{ReporterConfig, TestConfig};
use ferridriver_test::model::{Attachment, AttachmentBody, TestFailure, TestId, TestOutcome, TestStatus};
use ferridriver_test::reporter::{Reporter, ReporterEvent, RunStatus, StepFinishedEvent, StepStartedEvent, api};

const PROJECT: &str = "chromium";

fn fixtures_dir() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../../tests/fixtures")
    .canonicalize()
    .expect("tests/fixtures")
}

fn scratch(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("ferridriver-js-reporter-{name}-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).expect("scratch dir");
  // macOS puts the temp dir behind a `/var` -> `/private/var` symlink,
  // and the script sandbox compares canonical paths.
  dir.canonicalize().expect("scratch dir resolves")
}

fn entry(fixture: &str, options: BTreeMap<String, serde_json::Value>) -> ReporterConfig {
  ReporterConfig {
    name: fixtures_dir().join(fixture).display().to_string(),
    options,
  }
}

fn options(pairs: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
  pairs
    .iter()
    .map(|(key, value)| ((*key).to_string(), value.clone()))
    .collect()
}

async fn reporter_for(
  fixture: &str,
  options: BTreeMap<String, serde_json::Value>,
  cwd: &Path,
) -> ferridriver_script::JsReporter {
  let config = TestConfig::default();
  let module = ferridriver_script::reporter::load(
    &entry(fixture, options),
    &config,
    cwd,
    ferridriver_script::ScriptCaps::default(),
  )
  .await
  .expect("reporter loads");
  Arc::new(module).reporter()
}

// ── A synthetic run ──

fn test_id(name: &str) -> TestId {
  TestId {
    file: "tests/pay.spec.ts".to_string(),
    suite: Some("tests/pay.spec.ts::Checkout".to_string()),
    name: name.to_string(),
    line: Some(12),
    column: Some(3),
  }
}

fn case_of(name: &str) -> api::Case {
  let id = test_id(name);
  api::Case {
    id: id.stable_id(PROJECT),
    title: name.to_string(),
    title_path: vec![
      String::new(),
      PROJECT.to_string(),
      "tests/pay.spec.ts".to_string(),
      "Checkout".to_string(),
      name.to_string(),
    ],
    location: api::Location {
      file: "tests/pay.spec.ts".to_string(),
      line: 12,
      column: 3,
    },
    expected_status: "passed".to_string(),
    timeout: 30_000,
    retries: 0,
    repeat_each_index: 0,
    tags: vec!["@smoke".to_string()],
    annotations: Vec::new(),
    project_name: PROJECT.to_string(),
  }
}

fn preamble(names: &[&str]) -> Arc<api::RunPreamble> {
  let describe = api::Suite {
    title: "Checkout".to_string(),
    kind: api::SuiteKind::Describe,
    title_path: vec![
      String::new(),
      PROJECT.to_string(),
      "tests/pay.spec.ts".to_string(),
      "Checkout".to_string(),
    ],
    location: None,
    project: None,
    suites: Vec::new(),
    tests: names.iter().map(|name| case_of(name)).collect(),
  };
  let file = api::Suite {
    title: "tests/pay.spec.ts".to_string(),
    kind: api::SuiteKind::File,
    title_path: vec![String::new(), PROJECT.to_string(), "tests/pay.spec.ts".to_string()],
    location: Some(api::Location {
      file: "tests/pay.spec.ts".to_string(),
      line: 0,
      column: 0,
    }),
    project: None,
    suites: vec![describe],
    tests: Vec::new(),
  };
  let project = api::Suite {
    title: PROJECT.to_string(),
    kind: api::SuiteKind::Project,
    title_path: vec![String::new(), PROJECT.to_string()],
    location: None,
    project: Some(serde_json::json!({ "name": PROJECT, "id": PROJECT })),
    suites: vec![file],
    tests: Vec::new(),
  };
  Arc::new(api::RunPreamble {
    config: serde_json::json!({ "rootDir": "/repo", "projects": [{ "name": PROJECT }] }),
    suite: api::Suite {
      title: String::new(),
      kind: api::SuiteKind::Root,
      title_path: vec![String::new()],
      location: None,
      project: None,
      suites: vec![project],
      tests: Vec::new(),
    },
  })
}

fn outcome(name: &str, status: TestStatus) -> Arc<TestOutcome> {
  Arc::new(TestOutcome {
    case_metadata: None,
    test_id: test_id(name),
    status,
    duration: Duration::from_millis(120),
    attempt: 1,
    max_attempts: 1,
    error: (status != TestStatus::Passed).then(|| TestFailure::from("it did not add up".to_string())),
    errors: Vec::new(),
    attachments: vec![Attachment {
      name: "screenshot".to_string(),
      content_type: "image/png".to_string(),
      body: AttachmentBody::Bytes(vec![1, 2, 3, 4]),
      step_id: None,
    }],
    steps: Vec::new(),
    stdout: String::new(),
    stderr: String::new(),
    annotations: Vec::new(),
    metadata: serde_json::Value::Null,
    project_name: PROJECT.to_string(),
    worker_index: 0,
    parallel_index: 0,
    start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    expected_status: ferridriver_test::model::ExpectedStatus::Pass,
    timeout: Duration::from_secs(30),
  })
}

/// One test running one step, start to finish.
async fn drive(reporter: &mut dyn Reporter, names: &[&str], status: TestStatus) {
  reporter
    .on_event(&ReporterEvent::RunStarted {
      total_tests: names.len(),
      num_workers: 1,
      metadata: serde_json::Value::Null,
      start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
      preamble: preamble(names),
    })
    .await;
  for name in names {
    reporter
      .on_event(&ReporterEvent::TestStarted {
        test_id: test_id(name),
        project: PROJECT.to_string(),
        attempt: 1,
        worker_id: 0,
      })
      .await;
    reporter
      .on_event(&ReporterEvent::StepStarted(Arc::new(StepStartedEvent {
        test_id: test_id(name),
        project: PROJECT.to_string(),
        step_id: "step-1".to_string(),
        parent_step_id: None,
        title: "open the cart".to_string(),
        category: ferridriver_test::model::StepCategory::TestStep,
        location: None,
      })))
      .await;
    reporter
      .on_event(&ReporterEvent::TestOutput(Arc::new(
        ferridriver_test::reporter::TestOutputEvent {
          test_id: test_id(name),
          project: PROJECT.to_string(),
          stderr: false,
          text: "a line the test printed".to_string(),
        },
      )))
      .await;
    reporter
      .on_event(&ReporterEvent::StepFinished(Arc::new(StepFinishedEvent {
        test_id: test_id(name),
        project: PROJECT.to_string(),
        step_id: "step-1".to_string(),
        title: "open the cart".to_string(),
        category: ferridriver_test::model::StepCategory::TestStep,
        duration: Duration::from_millis(40),
        error: None,
        metadata: None,
        annotations: Vec::new(),
      })))
      .await;
    reporter
      .on_event(&ReporterEvent::TestFinished {
        outcome: outcome(name, status),
      })
      .await;
  }
  reporter
    .on_event(&ReporterEvent::RunFinished {
      total: names.len(),
      passed: names.len(),
      failed: 0,
      skipped: 0,
      flaky: 0,
      duration: Duration::from_millis(900),
      status: RunStatus::Passed,
    })
    .await;
  let _ = reporter.finalize().await;
}

fn read_summary(dir: &Path, name: &str) -> serde_json::Value {
  let path = dir.join(name);
  let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
  serde_json::from_str(&text).expect("summary is JSON")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_v1_reporter_sees_playwright_shapes_in_every_hook() {
  let dir = scratch("counting");
  let summary_path = dir.join("summary.json");
  let mut reporter = reporter_for(
    "counting-reporter.ts",
    options(&[("outputFile", summary_path.display().to_string().into())]),
    &dir,
  )
  .await;
  drive(&mut reporter, &["adds a row"], TestStatus::Passed).await;

  let summary = read_summary(&dir, "summary.json");
  let calls = &summary["calls"];
  assert_eq!(calls["onBegin"], 1, "{summary}");
  assert_eq!(calls["onTestBegin"], 1, "{summary}");
  assert_eq!(calls["onStepBegin"], 1, "{summary}");
  assert_eq!(calls["onStepEnd"], 1, "{summary}");
  assert_eq!(calls["onTestEnd"], 1, "{summary}");
  assert_eq!(calls["onStdOut"], 1, "{summary}");
  assert_eq!(calls["onEnd"], 1, "{summary}");

  // A V1 reporter is never configured — that is the whole point of the
  // wrapper, and the bug it was ported to prevent.
  assert_eq!(summary["configuredCalled"], false, "{summary}");
  assert_eq!(summary["configRootDir"], "/repo", "{summary}");
  assert_eq!(summary["configProjects"][0], PROJECT, "{summary}");

  assert_eq!(summary["suiteType"], "root", "{summary}");
  assert_eq!(summary["suiteTitlePath"], serde_json::json!([""]), "{summary}");
  assert_eq!(
    summary["allTests"],
    serde_json::json!([" > chromium > tests/pay.spec.ts > Checkout > adds a row"]),
    "{summary}"
  );
  assert_eq!(summary["entryTypes"], serde_json::json!(["project"]), "{summary}");
  assert_eq!(summary["firstProject"], PROJECT, "{summary}");
  assert_eq!(
    summary["testProject"], PROJECT,
    "the case reaches its project through parents: {summary}"
  );

  assert_eq!(summary["statuses"], serde_json::json!(["passed"]), "{summary}");
  assert_eq!(summary["outcomes"], serde_json::json!(["expected"]), "{summary}");
  assert_eq!(summary["ok"], serde_json::json!([true]), "{summary}");
  assert_eq!(summary["stepTitles"], serde_json::json!(["open the cart"]), "{summary}");
  assert_eq!(
    summary["stepPaths"],
    serde_json::json!([["open the cart"]]),
    "{summary}"
  );
  assert_eq!(
    summary["stdout"],
    serde_json::json!(["a line the test printed"]),
    "{summary}"
  );

  let attachment = &summary["attachments"][0];
  assert_eq!(attachment["name"], "screenshot", "{summary}");
  assert_eq!(attachment["contentType"], "image/png", "{summary}");
  assert_eq!(
    attachment["body"], "AQIDBA==",
    "the bytes round-trip as a Buffer: {summary}"
  );

  assert_eq!(summary["status"], "passed", "{summary}");
  assert_eq!(summary["durationIsNumber"], true, "{summary}");
  assert_eq!(summary["startTimeIsDate"], true, "{summary}");
  assert_eq!(summary["errors"], serde_json::json!([]), "{summary}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unexpected_failure_reads_as_unexpected() {
  let dir = scratch("failing");
  let summary_path = dir.join("summary.json");
  let mut reporter = reporter_for(
    "counting-reporter.ts",
    options(&[("outputFile", summary_path.display().to_string().into())]),
    &dir,
  )
  .await;
  drive(&mut reporter, &["adds a row"], TestStatus::Failed).await;

  let summary = read_summary(&dir, "summary.json");
  assert_eq!(summary["statuses"], serde_json::json!(["failed"]), "{summary}");
  assert_eq!(summary["outcomes"], serde_json::json!(["unexpected"]), "{summary}");
  assert_eq!(summary["ok"], serde_json::json!([false]), "{summary}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_v2_reporter_is_configured_then_begun_with_the_suite_alone() {
  let dir = scratch("v2");
  let summary_path = dir.join("v2.json");
  let mut reporter = reporter_for(
    "v2-reporter.ts",
    options(&[("outputFile", summary_path.display().to_string().into())]),
    &dir,
  )
  .await;
  drive(&mut reporter, &["adds a row"], TestStatus::Passed).await;

  let summary = read_summary(&dir, "v2.json");
  assert_eq!(summary["rootDir"], "/repo", "onConfigure carries the config: {summary}");
  assert_eq!(
    summary["beganType"], "root",
    "onBegin's argument is the SUITE: {summary}"
  );
  assert_eq!(summary["beganWith"], "1", "{summary}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_throwing_reporter_is_isolated() {
  let dir = scratch("throwing");
  let mut throwing = reporter_for("throwing-reporter.ts", BTreeMap::new(), &dir).await;
  drive(&mut throwing, &["adds a row"], TestStatus::Passed).await;

  // The proof it is isolated rather than merely tolerated: a second
  // reporter driven through the same events still gets all of them.
  let summary_path = dir.join("summary.json");
  let mut counting = reporter_for(
    "counting-reporter.ts",
    options(&[("outputFile", summary_path.display().to_string().into())]),
    &dir,
  )
  .await;
  drive(&mut counting, &["adds a row"], TestStatus::Passed).await;
  let summary = read_summary(&dir, "summary.json");
  assert_eq!(summary["calls"]["onEnd"], 1, "{summary}");
}

#[tokio::test(flavor = "multi_thread")]
async fn prints_to_stdio_is_the_module_s_own_answer() {
  let dir = scratch("prints");
  let quiet = reporter_for(
    "counting-reporter.ts",
    options(&[("outputFile", dir.join("a.json").display().to_string().into())]),
    &dir,
  )
  .await;
  assert!(!quiet.prints_to_stdio(), "the fixture answers false by default");

  let loud = reporter_for(
    "counting-reporter.ts",
    options(&[
      ("outputFile", dir.join("b.json").display().to_string().into()),
      ("printsToStdio", true.into()),
    ]),
    &dir,
  )
  .await;
  assert!(loud.prints_to_stdio(), "and true when its options say so");

  // A reporter that declares no `printsToStdio` at all defaults to
  // true, as Playwright's wrapper does.
  let bare = reporter_for("throwing-reporter.ts", BTreeMap::new(), &dir).await;
  assert!(bare.prints_to_stdio(), "a missing printsToStdio defaults to true");
}

#[tokio::test(flavor = "multi_thread")]
async fn preprocess_edits_reach_the_runner() {
  let dir = scratch("preprocess");
  let mut reporter = reporter_for(
    "preprocess-reporter.ts",
    options(&[("outputFile", dir.join("pre.json").display().to_string().into())]),
    &dir,
  )
  .await;
  let preamble = preamble(&["an excluded case", "a skipped case", "a plain case"]);
  let mut edits = ferridriver_test::reporter::TestRunEdits::default();
  reporter
    .preprocess(&preamble, &mut edits)
    .await
    .expect("preprocess succeeds");

  let seen = read_summary(&dir, "pre.json");
  assert_eq!(
    seen["seen"],
    serde_json::json!(["an excluded case", "a skipped case", "a plain case"]),
    "the reporter walks the whole corpus: {seen}"
  );
  assert_eq!(
    edits.excluded,
    vec![test_id("an excluded case").stable_id(PROJECT)],
    "exclude names the case by its stable id"
  );
  assert_eq!(edits.annotations.len(), 1, "{:?}", edits.annotations);
  assert_eq!(edits.annotations[0].0, test_id("a skipped case").stable_id(PROJECT));
  assert!(
    matches!(
      &edits.annotations[0].1,
      ferridriver_test::model::TestAnnotation::Skip { reason: Some(reason), .. } if reason == "a reporter said so"
    ),
    "{:?}",
    edits.annotations[0].1
  );
  assert!(edits.skip_sharding, "skipSharding() is recorded");
}

#[tokio::test(flavor = "multi_thread")]
async fn on_end_returning_a_status_decides_the_run() {
  let dir = scratch("status");
  let mut reporter = reporter_for("status-reporter.ts", options(&[("status", "failed".into())]), &dir).await;
  drive(&mut reporter, &["adds a row"], TestStatus::Passed).await;
  assert_eq!(
    reporter.status_override(),
    Some(RunStatus::Failed),
    "the reporter's onEnd overrides a passing run",
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reporter_that_does_not_resolve_fails_at_load() {
  let dir = scratch("missing");
  let config = TestConfig::default();
  let error = ferridriver_script::reporter::load(
    &ReporterConfig {
      name: "./no-such-reporter.ts".to_string(),
      options: BTreeMap::new(),
    },
    &config,
    &dir,
    ferridriver_script::ScriptCaps::default(),
  )
  .await;
  let Err(error) = error else {
    panic!("a missing reporter module is an error, not a silent skip")
  };
  assert!(error.message.contains("no-such-reporter.ts"), "{}", error.message);
}
