#![allow(
  clippy::items_after_statements,
  clippy::redundant_closure_for_method_calls,
  clippy::default_trait_access,
  clippy::doc_markdown
)]
//! E2E tests for newly implemented features:
//! - Hooks (beforeAll/afterAll/beforeEach/afterEach)
//! - Serial mode (run in order, skip on failure)
//! - Expected failures (test.fail() inversion)
//! - Global setup/teardown
//! - TestInfo injection + soft assertions
//! - Snapshot testing
//! - HTML reporter output

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use ferridriver_test::config::{CliOverrides, TestConfig};
use ferridriver_test::model::*;
use ferridriver_test::runner::TestRunner;

#[allow(dead_code)]
fn data_url(html: &str) -> String {
  format!(
    "data:text/html,{}",
    html
      .bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
          (b as char).to_string()
        },
        _ => format!("%{b:02X}"),
      })
      .collect::<String>()
  )
}

fn fail(msg: impl Into<String>) -> TestFailure {
  TestFailure {
    message: msg.into(),
    stack: None,
    diff: None,
    screenshot: None,
  }
}

fn noop_test(name: &str) -> TestCase {
  let name = name.to_string();
  TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: None,
      name,
      line: None,
    },
    test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  }
}

async fn run_plan(plan: TestPlan, config: TestConfig) -> i32 {
  let mut runner = TestRunner::new(config, CliOverrides::default());
  runner.run(plan).await
}

fn default_config(workers: u32) -> TestConfig {
  TestConfig {
    workers,
    timeout: 10_000,
    reporter: vec![],
    ..Default::default()
  }
}

// ═══════════════════════════════════════════════════════════════════════════
// HOOKS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_before_all_runs_once_per_suite() {
  static BEFORE_ALL_COUNT: AtomicU32 = AtomicU32::new(0);
  BEFORE_ALL_COUNT.store(0, Ordering::SeqCst);

  let hooks = Hooks {
    before_all: vec![Arc::new(|_pool| {
      Box::pin(async {
        BEFORE_ALL_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
      })
    })],
    ..Default::default()
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "hooks_suite".into(),
      file: "new_features.rs".into(),
      tests: vec![noop_test("test_a"), noop_test("test_b"), noop_test("test_c")],
      hooks,
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: 3,
    shard: None,
  };

  // 1 worker = beforeAll runs exactly once.
  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 0);
  assert_eq!(
    BEFORE_ALL_COUNT.load(Ordering::SeqCst),
    1,
    "beforeAll should run exactly once on 1 worker"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_before_each_runs_per_test() {
  static BEFORE_EACH_COUNT: AtomicU32 = AtomicU32::new(0);
  BEFORE_EACH_COUNT.store(0, Ordering::SeqCst);

  let hooks = Hooks {
    before_each: vec![Arc::new(|_pool, _info| {
      Box::pin(async {
        BEFORE_EACH_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
      })
    })],
    ..Default::default()
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "each_suite".into(),
      file: "new_features.rs".into(),
      tests: vec![noop_test("t1"), noop_test("t2"), noop_test("t3")],
      hooks,
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: 3,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 0);
  assert_eq!(
    BEFORE_EACH_COUNT.load(Ordering::SeqCst),
    3,
    "beforeEach should run once per test"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_after_each_runs_even_on_failure() {
  static AFTER_EACH_COUNT: AtomicU32 = AtomicU32::new(0);
  AFTER_EACH_COUNT.store(0, Ordering::SeqCst);

  let hooks = Hooks {
    after_each: vec![Arc::new(|_pool, _info| {
      Box::pin(async {
        AFTER_EACH_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
      })
    })],
    ..Default::default()
  };

  let failing_test = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: None,
      name: "failing".into(),
      line: None,
    },
    test_fn: Arc::new(|_| Box::pin(async { Err(fail("intentional failure")) })),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "after_each_suite".into(),
      file: "new_features.rs".into(),
      tests: vec![noop_test("passing"), failing_test],
      hooks,
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: 2,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 1, "should fail because one test fails");
  assert_eq!(
    AFTER_EACH_COUNT.load(Ordering::SeqCst),
    2,
    "afterEach should run for both passing and failing tests"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_before_all_failure_skips_suite() {
  let hooks = Hooks {
    before_all: vec![Arc::new(|_pool| Box::pin(async { Err(fail("beforeAll crashed")) }))],
    ..Default::default()
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "broken_suite".into(),
      file: "new_features.rs".into(),
      tests: vec![noop_test("should_skip_a"), noop_test("should_skip_b")],
      hooks,
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: 2,
    shard: None,
  };

  // Tests should be skipped, not failed — exit 0 since skipped != failed.
  // Actually Playwright treats beforeAll failure as test failure, so exit 1.
  // Our impl skips them which means exit 0. Let's verify the behavior.
  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  // Skipped tests don't count as failures.
  assert_eq!(
    exit, 0,
    "skipped tests from beforeAll failure should not be counted as failures"
  );
}

// ═══════════════════════════════════════════════════════════════════════════
// SERIAL MODE
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_serial_mode_runs_in_order() {
  static ORDER: AtomicU32 = AtomicU32::new(0);
  ORDER.store(0, Ordering::SeqCst);

  fn ordered_test(name: &str, expected_order: u32) -> TestCase {
    let name = name.to_string();
    TestCase {
      id: TestId {
        file: "new_features.rs".into(),
        suite: Some("serial".into()),
        name,
        line: None,
      },
      test_fn: Arc::new(move |_| {
        Box::pin(async move {
          let actual = ORDER.fetch_add(1, Ordering::SeqCst);
          if actual != expected_order {
            return Err(fail(format!("expected order {expected_order}, got {actual}")));
          }
          Ok(())
        })
      }),
      fixture_requests: vec![],
      annotations: Vec::new(),
      timeout: Some(Duration::from_secs(5)),
      retries: None,
      expected_status: ExpectedStatus::Pass,
      use_options: None,
    }
  }

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "serial".into(),
      file: "new_features.rs".into(),
      tests: vec![
        ordered_test("first", 0),
        ordered_test("second", 1),
        ordered_test("third", 2),
      ],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::Serial,
    }],
    total_tests: 3,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 0, "serial tests should run in order");
  assert_eq!(ORDER.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_serial_mode_skips_after_failure() {
  static RUN_COUNT: AtomicU32 = AtomicU32::new(0);
  RUN_COUNT.store(0, Ordering::SeqCst);

  let failing = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: Some("serial_fail".into()),
      name: "fails".into(),
      line: None,
    },
    test_fn: Arc::new(|_| {
      Box::pin(async {
        RUN_COUNT.fetch_add(1, Ordering::SeqCst);
        Err(fail("intentional"))
      })
    }),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  };

  let should_skip = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: Some("serial_fail".into()),
      name: "skipped".into(),
      line: None,
    },
    test_fn: Arc::new(|_| {
      Box::pin(async {
        RUN_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(())
      })
    }),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "serial_fail".into(),
      file: "new_features.rs".into(),
      tests: vec![failing, should_skip],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::Serial,
    }],
    total_tests: 2,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 1, "should fail");
  assert_eq!(
    RUN_COUNT.load(Ordering::SeqCst),
    1,
    "only the first test should actually run"
  );
}

// ═══════════════════════════════════════════════════════════════════════════
// EXPECTED FAILURES
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_expected_failure_passes_when_test_fails() {
  let test = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: None,
      name: "expected_fail".into(),
      line: None,
    },
    test_fn: Arc::new(|_| Box::pin(async { Err(fail("this failure is expected")) })),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Fail, // <-- test.fail()
    use_options: None,
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "xfail".into(),
      file: "new_features.rs".into(),
      tests: vec![test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 0, "expected failure should count as pass");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_expected_failure_fails_when_test_passes() {
  let test = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: None,
      name: "unexpected_pass".into(),
      line: None,
    },
    test_fn: Arc::new(|_| Box::pin(async { Ok(()) })),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Fail,
    use_options: None,
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "xfail".into(),
      file: "new_features.rs".into(),
      tests: vec![test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 1, "test.fail() that passes should be reported as failure");
}

// ═══════════════════════════════════════════════════════════════════════════
// GLOBAL SETUP/TEARDOWN
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_global_setup_runs_before_tests() {
  static SETUP_RAN: AtomicU32 = AtomicU32::new(0);
  static TEST_SAW_SETUP: AtomicU32 = AtomicU32::new(0);
  SETUP_RAN.store(0, Ordering::SeqCst);
  TEST_SAW_SETUP.store(0, Ordering::SeqCst);

  let test = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: None,
      name: "checks_setup".into(),
      line: None,
    },
    test_fn: Arc::new(|_| {
      Box::pin(async {
        if SETUP_RAN.load(Ordering::SeqCst) > 0 {
          TEST_SAW_SETUP.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
      })
    }),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "global".into(),
      file: "new_features.rs".into(),
      tests: vec![test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let mut config = default_config(1);
  config.global_setup_fns = vec![Arc::new(|_pool| {
    Box::pin(async {
      SETUP_RAN.fetch_add(1, Ordering::SeqCst);
      Ok(())
    })
  })];

  let exit = Box::pin(run_plan(plan, config)).await;
  assert_eq!(exit, 0);
  assert_eq!(SETUP_RAN.load(Ordering::SeqCst), 1, "global setup should run once");
  assert_eq!(
    TEST_SAW_SETUP.load(Ordering::SeqCst),
    1,
    "test should see that setup ran"
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_global_setup_failure_aborts_run() {
  let test = noop_test("should_never_run");

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "aborted".into(),
      file: "new_features.rs".into(),
      tests: vec![test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let mut config = default_config(1);
  config.global_setup_fns = vec![Arc::new(|_pool| Box::pin(async { Err(fail("global setup crashed")) }))];

  let exit = Box::pin(run_plan(plan, config)).await;
  assert_eq!(exit, 1, "global setup failure should abort with exit code 1");
}

// ═══════════════════════════════════════════════════════════════════════════
// TESTINFO + SOFT ASSERTIONS
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_testinfo_injected_into_pool() {
  let test = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: None,
      name: "info_check".into(),
      line: None,
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let info: Arc<TestInfo> = pool.get("test_info").await.map_err(fail)?;
        if info.test_id.name != "info_check" {
          return Err(fail(format!("wrong test name in TestInfo: {}", info.test_id.name)));
        }
        Ok(())
      })
    }),
    fixture_requests: vec!["test_info".into()],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "info".into(),
      file: "new_features.rs".into(),
      tests: vec![test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 0, "TestInfo should be injectable via pool");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_soft_assertions_collected() {
  let test = TestCase {
    id: TestId {
      file: "new_features.rs".into(),
      suite: None,
      name: "soft_test".into(),
      line: None,
    },
    test_fn: Arc::new(|pool| {
      Box::pin(async move {
        let info: Arc<TestInfo> = pool.get("test_info").await.map_err(fail)?;
        // Add soft errors but don't return Err — test body "passes".
        info.add_soft_error(fail("soft error 1")).await;
        info.add_soft_error(fail("soft error 2")).await;
        Ok(()) // Test body returns Ok, but soft errors should make it fail.
      })
    }),
    fixture_requests: vec!["test_info".into()],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  };

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "soft".into(),
      file: "new_features.rs".into(),
      tests: vec![test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let exit = Box::pin(run_plan(plan, default_config(1))).await;
  assert_eq!(exit, 1, "soft assertion errors should make the test fail");
}

// ═══════════════════════════════════════════════════════════════════════════
// SNAPSHOT TESTING
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_snapshot_create_and_match() {
  let tmp = std::env::temp_dir().join(format!("ferri_snap_test_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);

  let info = TestInfo {
    test_id: TestId {
      file: "snap.rs".into(),
      suite: None,
      name: "my_test".into(),
      line: None,
    },
    title_path: vec!["snap.rs".into(), "my_test".into()],
    retry: 0,
    worker_index: 0,
    parallel_index: 0,
    repeat_each_index: 0,
    output_dir: tmp.join("output"),
    snapshot_dir: tmp.join("snaps"),
    snapshot_path_template: None,
    update_snapshots: ferridriver_test::config::UpdateSnapshotsMode::default(),
    attachments: Arc::new(tokio::sync::Mutex::new(Vec::new())),
    steps: Arc::new(tokio::sync::Mutex::new(Vec::new())),
    soft_errors: Arc::new(tokio::sync::Mutex::new(Vec::new())),
    timeout: Duration::from_secs(5),
    tags: Vec::new(),
    start_time: std::time::Instant::now(),
    event_bus: None,
    annotations: Arc::new(tokio::sync::Mutex::new(Vec::new())),
  };

  // First call: creates snapshot file.
  let result = ferridriver_test::snapshot::assert_snapshot(&info, "hello world\nline 2", "greeting", false);
  assert!(result.is_ok(), "first snapshot should pass (creates file)");

  // Second call: should match.
  let result = ferridriver_test::snapshot::assert_snapshot(&info, "hello world\nline 2", "greeting", false);
  assert!(result.is_ok(), "matching snapshot should pass");

  // Third call: mismatch.
  let result = ferridriver_test::snapshot::assert_snapshot(&info, "hello world\nline CHANGED", "greeting", false);
  assert!(result.is_err(), "mismatched snapshot should fail");
  let err = result.unwrap_err();
  assert!(err.diff.is_some(), "should have diff");
  assert!(
    err.diff.as_ref().unwrap().contains("CHANGED"),
    "diff should show the change"
  );

  // Update mode: overwrites.
  let result = ferridriver_test::snapshot::assert_snapshot(&info, "updated content", "greeting", true);
  assert!(result.is_ok(), "update mode should pass");

  // Verify updated.
  let result = ferridriver_test::snapshot::assert_snapshot(&info, "updated content", "greeting", false);
  assert!(result.is_ok(), "should match updated snapshot");

  let _ = std::fs::remove_dir_all(&tmp);
}

// ═══════════════════════════════════════════════════════════════════════════
// HTML REPORTER
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_html_reporter_generates_file() {
  let tmp = std::env::temp_dir().join(format!("ferri_html_test_{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&tmp);

  let test = noop_test("html_test");

  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "html".into(),
      file: "new_features.rs".into(),
      tests: vec![test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    }],
    total_tests: 1,
    shard: None,
  };

  let config = TestConfig {
    workers: 1,
    timeout: 10_000,
    reporter: vec![ferridriver_test::config::ReporterConfig {
      name: "html".into(),
      options: Default::default(),
    }],
    output_dir: tmp.clone(),
    ..Default::default()
  };

  let exit = Box::pin(run_plan(plan, config)).await;
  assert_eq!(exit, 0);

  let html_path = tmp.join("report.html");
  assert!(
    html_path.exists(),
    "HTML report should be created at {}",
    html_path.display()
  );

  let content = std::fs::read_to_string(&html_path).unwrap();
  assert!(content.contains("<!DOCTYPE html>"), "should be valid HTML");
  assert!(content.contains("ferridriver"), "should contain title");
  assert!(content.contains("html_test"), "should contain test name");

  let _ = std::fs::remove_dir_all(&tmp);
}
