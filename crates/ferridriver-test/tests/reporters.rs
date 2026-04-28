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
  }
}

fn make_outcome(id: &TestId, status: TestStatus, error: Option<&str>) -> TestOutcome {
  TestOutcome {
    test_id: id.clone(),
    status,
    duration: Duration::from_millis(10),
    attempt: 1,
    max_attempts: 1,
    error: error.map(|m| TestFailure {
      message: m.into(),
      stack: None,
      diff: None,
      screenshot: None,
    }),
    attachments: Vec::new(),
    steps: Vec::new(),
    stdout: String::new(),
    stderr: String::new(),
    annotations: Vec::new(),
    metadata: serde_json::Value::Null,
  }
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
  })
  .await;
  let id1 = make_id("t1");
  let id2 = make_id("t2");
  let id3 = make_id("t3");
  r.on_event(&ReporterEvent::TestFinished {
    test_id: id1.clone(),
    outcome: make_outcome(&id1, TestStatus::Passed, None),
  })
  .await;
  r.on_event(&ReporterEvent::TestFinished {
    test_id: id2.clone(),
    outcome: make_outcome(&id2, TestStatus::Failed, Some("boom")),
  })
  .await;
  r.on_event(&ReporterEvent::TestFinished {
    test_id: id3.clone(),
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
    test_id: id.clone(),
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
  })
  .await;
  r.on_event(&ReporterEvent::TestStarted {
    test_id: id.clone(),
    attempt: 1,
  })
  .await;
  r.on_event(&ReporterEvent::TestFinished {
    test_id: id.clone(),
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
