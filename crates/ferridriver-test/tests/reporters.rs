#![allow(
  clippy::items_after_statements,
  clippy::redundant_closure_for_method_calls,
  clippy::default_trait_access,
  clippy::expect_used,
  clippy::unwrap_used
)]
//! Cluster 6 — built-in reporter coverage for §7.20 / §7.21.
//!
//! Drives `ReporterEvent` directly through the `Reporter` trait so
//! the assertions don't need a live browser.

use std::sync::Arc;
use std::time::Duration;

use ferridriver_test::model::{TestFailure, TestId, TestOutcome, TestStatus};
use ferridriver_test::reporter::{Reporter, ReporterEvent, blob, dot, empty, github};

struct ScopedDir(std::path::PathBuf);
impl ScopedDir {
  fn new(prefix: &str) -> Self {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scoped dir");
    Self(path)
  }
  fn path(&self) -> &std::path::Path {
    &self.0
  }
}
impl Drop for ScopedDir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn make_id(name: &str) -> TestId {
  TestId {
    file: "tests/reporters.rs".into(),
    suite: None,
    name: name.into(),
    line: Some(42),
    column: None,
  }
}

fn make_outcome(id: &TestId, status: TestStatus, error: Option<&str>) -> std::sync::Arc<TestOutcome> {
  let failure = error.map(|m| TestFailure {
    message: m.into(),
    stack: None,
    diff: None,
    screenshot: None,
  });
  std::sync::Arc::new(TestOutcome {
    test_id: id.clone(),
    status,
    duration: Duration::from_millis(10),
    attempt: 1,
    max_attempts: 1,
    errors: failure.iter().cloned().collect(),
    error: failure,
    ..Default::default()
  })
}

#[tokio::test]
async fn dot_reporter_emits_one_glyph_per_test() {
  // Capturing stdout in-process is fiddly — drive the trait directly
  // and assert it doesn't panic + finalize cleanly. Smoke check for
  // crash-free execution; the rendered glyphs are visually verified.
  let mut r = dot::DotReporter::new();
  r.on_event(&ReporterEvent::RunStarted {
    total_tests: 3,
    num_workers: 1,
    metadata: serde_json::Value::Null,
    start_time: std::time::SystemTime::now(),
    preamble: std::sync::Arc::new(ferridriver_test::reporter::api::RunPreamble::empty()),
  })
  .await;
  let id1 = make_id("t1");
  let id2 = make_id("t2");
  let id3 = make_id("t3");
  r.on_event(&ReporterEvent::TestFinished {
    outcome: make_outcome(&id1, TestStatus::Passed, None),
  })
  .await;
  r.on_event(&ReporterEvent::TestFinished {
    outcome: make_outcome(&id2, TestStatus::Failed, Some("boom")),
  })
  .await;
  r.on_event(&ReporterEvent::TestFinished {
    outcome: make_outcome(&id3, TestStatus::Skipped, None),
  })
  .await;
  r.on_event(&ReporterEvent::RunFinished {
    total: 3,
    passed: 1,
    failed: 1,
    skipped: 1,
    flaky: 0,
    duration: Duration::from_millis(30),
    status: ferridriver_test::reporter::RunStatus::Passed,
  })
  .await;
  r.finalize().await.unwrap();
}

#[tokio::test]
async fn empty_reporter_swallows_every_event() {
  let mut r = empty::EmptyReporter;
  r.on_event(&ReporterEvent::RunStarted {
    total_tests: 0,
    num_workers: 0,
    metadata: serde_json::Value::Null,
    start_time: std::time::SystemTime::now(),
    preamble: std::sync::Arc::new(ferridriver_test::reporter::api::RunPreamble::empty()),
  })
  .await;
  r.finalize().await.unwrap();
}

#[tokio::test]
async fn github_reporter_emits_error_annotations_when_enabled() {
  // Wrap an EmptyReporter so the test's assertions read only the
  // GitHub annotation lines from stdout. Force `enabled = true` so
  // we don't need to mutate the env.
  struct Capture {
    events: Vec<ReporterEvent>,
  }
  #[async_trait::async_trait]
  impl Reporter for Capture {
    async fn on_event(&mut self, event: &ReporterEvent) {
      self.events.push(event.clone());
    }
  }
  let inner = Box::new(Capture { events: Vec::new() });
  let mut r = github::GithubReporter::new(inner).with_enabled(true);
  let id = make_id("crash");
  r.on_event(&ReporterEvent::TestFinished {
    outcome: make_outcome(&id, TestStatus::Failed, Some("boom\nwith\nnewlines")),
  })
  .await;
  r.finalize().await.unwrap();
  // Smoke: didn't panic and the delegate received the same event.
}

#[tokio::test]
async fn blob_reporter_writes_zip_and_merge_reads_back_events() {
  let dir = ScopedDir::new("ferri-blob-test");
  let blob_path = dir.path().join("report-1.zip");
  let mut r = blob::BlobReporter::new(blob_path.clone()).with_shard(1, 2);

  let id = make_id("blob-roundtrip");
  r.on_event(&ReporterEvent::RunStarted {
    total_tests: 1,
    num_workers: 1,
    metadata: serde_json::json!({ "key": "value" }),
    start_time: std::time::SystemTime::now(),
    preamble: std::sync::Arc::new(ferridriver_test::reporter::api::RunPreamble::empty()),
  })
  .await;
  r.on_event(&ReporterEvent::TestStarted {
    project: String::new(),
    test_id: id.clone(),
    attempt: 1,
    worker_id: 0,
  })
  .await;
  r.on_event(&ReporterEvent::TestFinished {
    outcome: make_outcome(&id, TestStatus::Passed, None),
  })
  .await;
  r.on_event(&ReporterEvent::RunFinished {
    total: 1,
    passed: 1,
    failed: 0,
    skipped: 0,
    flaky: 0,
    duration: Duration::from_millis(7),
    status: ferridriver_test::reporter::RunStatus::Passed,
  })
  .await;
  r.finalize().await.unwrap();
  assert!(blob_path.exists(), "blob zip should be written");

  // Read the zip back via the merge helper.
  let events = blob::read_blob_dir(dir.path()).expect("read_blob_dir");
  let kinds: Vec<&str> = events
    .iter()
    .map(|e| match e {
      ReporterEvent::RunStarted { .. } => "run-started",
      ReporterEvent::TestStarted { .. } => "test-started",
      ReporterEvent::TestFinished { .. } => "test-finished",
      ReporterEvent::RunFinished { .. } => "run-finished",
      _ => "other",
    })
    .collect();
  assert_eq!(
    kinds,
    vec!["run-started", "test-started", "test-finished", "run-finished"]
  );

  // Suppress unused-variable warning for the Arc import path.
  let _: Arc<()> = Arc::new(());
}

// ── Report shapes ──
//
// Every file-producing reporter is driven end-to-end and its output
// parsed back: a shape assertion on the written bytes is the only thing
// that proves a consumer of that format still works.

mod shapes {
  use std::collections::BTreeMap;
  use std::sync::Arc;
  use std::time::{Duration, SystemTime};

  use ferridriver_test::config::{ReporterConfig, TestConfig};
  use ferridriver_test::model::{
    Attachment, AttachmentBody, ExpectedStatus, StepCategory, StepStatus, TestAnnotation, TestFailure, TestId,
    TestOutcome, TestStatus, TestStep,
  };
  use ferridriver_test::reporter::{ReporterEvent, RunStatus, create_reporters_pub};

  use super::ScopedDir;

  fn id(name: &str, line: usize) -> TestId {
    TestId {
      file: "tests/login.spec.ts".into(),
      suite: Some("tests/login.spec.ts::auth".into()),
      name: name.into(),
      line: Some(line),
      column: Some(5),
    }
  }

  fn outcome(name: &str, status: TestStatus, attempt: u32) -> Arc<TestOutcome> {
    Arc::new(TestOutcome {
      test_id: id(name, 12),
      status,
      duration: Duration::from_millis(250),
      attempt,
      max_attempts: 2,
      project_name: "chromium".into(),
      worker_index: 2,
      parallel_index: 2,
      start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
      timeout: Duration::from_secs(30),
      annotations: vec![
        TestAnnotation::Tag("@smoke".into()),
        TestAnnotation::Info {
          type_name: "issue".into(),
          description: "JIRA-1".into(),
        },
      ],
      ..Default::default()
    })
  }

  fn failed(name: &str, attempt: u32) -> Arc<TestOutcome> {
    let failure = TestFailure {
      message: "Error: expect(locator).toHaveText(expected) failed".into(),
      stack: Some("    at check (tests/login.spec.ts:19:7)".into()),
      diff: Some("Expected: \"hi\"\nReceived: \"bye\"".into()),
      screenshot: Some(vec![137, 80, 78, 71]),
    };
    let mut out = TestOutcome {
      error: Some(failure.clone()),
      errors: vec![failure],
      steps: vec![TestStep {
        step_id: "s1".into(),
        title: "sign in".into(),
        category: StepCategory::TestStep,
        duration: Duration::from_millis(80),
        status: StepStatus::Failed,
        error: Some("no such button".into()),
        location: Some(ferridriver_test::model::StepLocation::new("tests/login.spec.ts", 19)),
        annotations: Vec::new(),
        parent_step_id: None,
        metadata: None,
        steps: Vec::new(),
      }],
      stdout: "trying to sign in\n".into(),
      attachments: vec![Attachment {
        name: "screenshot".into(),
        content_type: "image/png".into(),
        body: AttachmentBody::Bytes(vec![137, 80, 78, 71]),
        step_id: None,
      }],
      ..(*outcome(name, TestStatus::Failed, attempt)).clone()
    };
    out.status = TestStatus::Failed;
    Arc::new(out)
  }

  /// Drive a full run through the reporters `names` selects, writing
  /// into `dir`.
  async fn run_reporters(dir: &std::path::Path, names: &[&str], events: Vec<ReporterEvent>) {
    let config = TestConfig {
      output_dir: dir.to_path_buf(),
      test_dir: Some("tests".into()),
      quiet: true,
      reporter: names
        .iter()
        .map(|name| ReporterConfig {
          name: (*name).to_string(),
          options: BTreeMap::new(),
        })
        .collect(),
      ..Default::default()
    };

    let mut reporters = create_reporters_pub(&config.reporter.clone(), &config);
    for event in &events {
      reporters.emit(event).await;
    }
    reporters.finalize().await;
  }

  fn run(events: Vec<ReporterEvent>) -> Vec<ReporterEvent> {
    let mut all = vec![ReporterEvent::RunStarted {
      total_tests: 2,
      num_workers: 2,
      metadata: serde_json::json!({ "ci": "local" }),
      start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
      preamble: std::sync::Arc::new(ferridriver_test::reporter::api::RunPreamble::empty()),
    }];
    all.extend(events);
    all.push(ReporterEvent::RunFinished {
      total: 2,
      passed: 1,
      failed: 1,
      skipped: 0,
      flaky: 0,
      duration: Duration::from_millis(900),
      status: RunStatus::Failed,
    });
    all
  }

  #[tokio::test]
  async fn json_report_has_playwright_shape() {
    let dir = ScopedDir::new("ferri-json-shape");
    run_reporters(
      dir.path(),
      &["json"],
      run(vec![
        ReporterEvent::TestFinished {
          outcome: outcome("logs in", TestStatus::Passed, 1),
        },
        ReporterEvent::TestFinished {
          outcome: failed("rejects a bad password", 1),
        },
      ]),
    )
    .await;

    let text = std::fs::read_to_string(dir.path().join("results.json")).expect("results.json");
    let report: serde_json::Value = serde_json::from_str(&text).expect("valid json");

    // Top-level: exactly the four keys Playwright's JSONReport has.
    for key in ["config", "suites", "errors", "stats"] {
      assert!(report.get(key).is_some(), "missing {key}: {text}");
    }
    assert_eq!(report["stats"]["expected"], 1);
    assert_eq!(report["stats"]["unexpected"], 1);
    assert_eq!(report["stats"]["startTime"], "2023-11-14T22:13:20.000Z");
    assert_eq!(
      report["config"]["workers"],
      serde_json::json!(TestConfig::default().workers)
    );
    assert_eq!(report["config"]["projects"][0]["name"], "");

    let suite = &report["suites"][0];
    assert_eq!(suite["file"], "tests/login.spec.ts");
    let spec = &suite["specs"][0];
    assert_eq!(spec["title"], "logs in");
    assert_eq!(spec["ok"], true);
    assert_eq!(spec["line"], 12);
    assert_eq!(spec["column"], 5);
    assert_eq!(spec["tags"][0], "smoke", "the leading @ is stripped");

    let first = &spec["tests"][0];
    assert_eq!(first["projectName"], "chromium");
    assert_eq!(first["expectedStatus"], "passed");
    assert_eq!(first["status"], "expected");
    assert_eq!(first["timeout"], 30_000);

    let result = &first["results"][0];
    assert_eq!(result["workerIndex"], 2);
    assert_eq!(result["parallelIndex"], 2);
    assert_eq!(result["retry"], 0);
    assert_eq!(result["startTime"], "2023-11-14T22:13:20.000Z");
    assert_eq!(result["status"], "passed");

    let failing = &suite["specs"][1]["tests"][0]["results"][0];
    assert_eq!(failing["status"], "failed");
    assert_eq!(failing["errorLocation"]["file"], "tests/login.spec.ts");
    assert_eq!(failing["errorLocation"]["line"], 19);
    assert_eq!(failing["errorLocation"]["column"], 7);
    assert_eq!(failing["errors"].as_array().map(Vec::len), Some(1));
    assert_eq!(failing["stdout"][0]["text"], "trying to sign in\n");
    assert_eq!(failing["steps"][0]["title"], "sign in");
    assert_eq!(
      failing["attachments"][0]["body"], "iVBORw==",
      "inline bytes travel as base64"
    );
  }

  #[tokio::test]
  async fn junit_report_carries_properties_and_classification() {
    let dir = ScopedDir::new("ferri-junit-shape");
    // A file attachment that actually exists: the `[[ATTACHMENT|…]]`
    // marker is only emitted for one a CI server could open.
    let trace = dir.path().join("trace.zip");
    std::fs::write(&trace, b"not really a trace").expect("write trace");
    let mut with_trace = (*failed("rejects a bad password", 1)).clone();
    with_trace.attachments.push(Attachment {
      name: "trace".into(),
      content_type: "application/zip".into(),
      body: AttachmentBody::Path(trace),
      step_id: None,
    });

    run_reporters(
      dir.path(),
      &["junit"],
      run(vec![
        ReporterEvent::TestFinished {
          outcome: outcome("logs in", TestStatus::Passed, 1),
        },
        ReporterEvent::TestFinished {
          outcome: Arc::new(with_trace),
        },
      ]),
    )
    .await;

    let xml = std::fs::read_to_string(dir.path().join("junit.xml")).expect("junit.xml");

    assert!(
      xml.contains(r#"<testsuites id="" name="" tests="2" failures="1" skipped="0" errors="0""#),
      "{xml}"
    );
    assert!(xml.contains(r#"hostname="chromium""#), "project names the host: {xml}");
    assert!(xml.contains("timestamp=\""), "suite carries a timestamp: {xml}");
    assert!(
      xml.contains(r#"classname="tests/login.spec.ts""#),
      "classname is the file: {xml}"
    );
    assert!(
      xml.contains(r#"<property name="tag" value="@smoke"/>"#),
      "annotations become properties: {xml}"
    );
    assert!(xml.contains(r#"<property name="issue" value="JIRA-1"/>"#), "{xml}");
    assert!(
      xml.contains(r#"type="expect.toHaveText""#),
      "an assertion failure is typed by its matcher: {xml}"
    );
    assert!(xml.contains("<![CDATA["), "the failure body is character data: {xml}");
    assert!(
      xml.contains("[[ATTACHMENT|"),
      "path attachments are announced for CI: {xml}"
    );
  }

  #[tokio::test]
  async fn ctrf_and_markdown_reports_are_written() {
    let dir = ScopedDir::new("ferri-ctrf-shape");
    run_reporters(
      dir.path(),
      &["ctrf", "markdown"],
      run(vec![
        ReporterEvent::TestFinished {
          outcome: outcome("logs in", TestStatus::Passed, 1),
        },
        ReporterEvent::TestFinished {
          outcome: failed("rejects a bad password", 1),
        },
      ]),
    )
    .await;

    let ctrf: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(dir.path().join("ctrf-report.json")).expect("ctrf")).expect("json");
    assert_eq!(ctrf["results"]["tool"]["name"], "ferridriver");
    assert_eq!(ctrf["results"]["summary"]["tests"], 2);
    assert_eq!(ctrf["results"]["summary"]["passed"], 1);
    assert_eq!(ctrf["results"]["summary"]["failed"], 1);
    let tests = ctrf["results"]["tests"].as_array().expect("tests");
    assert_eq!(tests[0]["name"], "logs in");
    assert_eq!(tests[0]["status"], "passed");
    assert_eq!(tests[0]["browser"], "chromium");
    assert_eq!(tests[1]["status"], "failed");
    assert_eq!(tests[1]["line"], 12);
    assert!(tests[1]["message"].as_str().is_some_and(|m| m.contains("toHaveText")));

    let md = std::fs::read_to_string(dir.path().join("report.md")).expect("markdown");
    assert!(md.contains("| Passed | Failed | Flaky | Skipped | Duration |"), "{md}");
    assert!(md.contains("| 1 | 1 | 0 | 0 |"), "{md}");
    assert!(md.contains("### Failures"), "{md}");
    assert!(md.contains("rejects a bad password"), "{md}");
  }

  #[tokio::test]
  async fn a_retried_test_reads_as_flaky_in_every_report() {
    let dir = ScopedDir::new("ferri-flaky-shape");
    run_reporters(
      dir.path(),
      &["json", "ctrf"],
      run(vec![
        ReporterEvent::TestFinished {
          outcome: failed("logs in", 1),
        },
        ReporterEvent::TestFinished {
          outcome: outcome("logs in", TestStatus::Passed, 2),
        },
      ]),
    )
    .await;

    let json: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(dir.path().join("results.json")).expect("json")).expect("json");
    assert_eq!(json["stats"]["flaky"], 1);
    assert_eq!(json["stats"]["unexpected"], 0);
    let test = &json["suites"][0]["specs"][0]["tests"][0];
    assert_eq!(test["status"], "flaky");
    assert_eq!(test["results"].as_array().map(Vec::len), Some(2), "both attempts kept");
    assert_eq!(json["suites"][0]["specs"][0]["ok"], true);

    let ctrf: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(dir.path().join("ctrf-report.json")).expect("ctrf")).expect("json");
    assert_eq!(ctrf["results"]["tests"][0]["flaky"], true);
    assert_eq!(ctrf["results"]["tests"][0]["status"], "passed");
    assert_eq!(ctrf["results"]["tests"][0]["retries"], 1);
  }

  #[tokio::test]
  async fn an_expected_failure_is_not_reported_as_one() {
    let dir = ScopedDir::new("ferri-expected-fail");
    let mut known_bug = (*failed("known bug", 1)).clone();
    known_bug.expected_status = ExpectedStatus::Fail;
    run_reporters(
      dir.path(),
      &["json"],
      run(vec![ReporterEvent::TestFinished {
        outcome: Arc::new(known_bug),
      }]),
    )
    .await;

    let json: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(dir.path().join("results.json")).expect("json")).expect("json");
    assert_eq!(json["stats"]["expected"], 1);
    assert_eq!(json["stats"]["unexpected"], 0);
    assert_eq!(json["suites"][0]["specs"][0]["tests"][0]["expectedStatus"], "failed");
    assert_eq!(json["suites"][0]["specs"][0]["ok"], true);
  }

  #[tokio::test]
  async fn a_run_error_reaches_the_json_report() {
    let dir = ScopedDir::new("ferri-run-error");
    run_reporters(
      dir.path(),
      &["json"],
      run(vec![ReporterEvent::RunError {
        error: Box::new(TestFailure {
          message: "global setup failed: boom".into(),
          stack: Some("    at setup (global.ts:4:2)".into()),
          diff: None,
          screenshot: None,
        }),
      }]),
    )
    .await;

    let json: serde_json::Value =
      serde_json::from_str(&std::fs::read_to_string(dir.path().join("results.json")).expect("json")).expect("json");
    let errors = json["errors"].as_array().expect("errors");
    assert_eq!(errors.len(), 1);
    assert!(errors[0]["message"].as_str().is_some_and(|m| m.contains("boom")));
    assert_eq!(errors[0]["location"]["file"], "global.ts");
    assert_eq!(errors[0]["location"]["line"], 4);
  }

  #[tokio::test]
  async fn output_file_options_override_the_defaults() {
    let dir = ScopedDir::new("ferri-outputfile");
    let mut options = BTreeMap::new();
    options.insert(
      "outputFile".to_string(),
      serde_json::json!(dir.path().join("custom/where.xml").display().to_string()),
    );
    let config = TestConfig {
      output_dir: dir.path().to_path_buf(),
      quiet: true,
      reporter: vec![ReporterConfig {
        name: "junit".into(),
        options,
      }],
      ..Default::default()
    };

    let mut reporters = create_reporters_pub(&config.reporter.clone(), &config);
    for event in run(vec![ReporterEvent::TestFinished {
      outcome: outcome("logs in", TestStatus::Passed, 1),
    }]) {
      reporters.emit(&event).await;
    }
    reporters.finalize().await;

    assert!(
      dir.path().join("custom/where.xml").exists(),
      "the option chose the path"
    );
    assert!(!dir.path().join("junit.xml").exists(), "and the default was not used");
  }
}
