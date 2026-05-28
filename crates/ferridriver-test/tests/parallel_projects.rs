//! Verifies the multi-project ready-set scheduler runs independent projects
//! concurrently (wall-clock ~= slowest project, not the sum) while a
//! parallelism cap of 1 still serializes them, and that a dependency edge
//! still orders the dependent strictly after its dependency.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferridriver_test::config::{CliOverrides, ProjectConfig, ReporterConfig, TestConfig};
use ferridriver_test::model::*;
use ferridriver_test::runner::TestRunner;

/// A fixture-free test that simply sleeps. Requesting no fixtures means the
/// worker never launches a browser, so the only cost is the sleep — making the
/// wall-clock assertions deterministic without a Chrome dependency.
fn sleeping_test(name: &str, sleep: Duration) -> TestCase {
  TestCase {
    id: TestId {
      file: "parallel_projects.rs".into(),
      suite: Some("proj".into()),
      name: name.into(),
      line: None,
    },
    test_fn: Arc::new(move |_pool| {
      Box::pin(async move {
        tokio::time::sleep(sleep).await;
        Ok(())
      })
    }),
    fixture_requests: Vec::new(),
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(30)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  }
}

fn plan_with(names: &[&str], sleep: Duration) -> TestPlan {
  let suites = names
    .iter()
    .map(|n| TestSuite {
      name: "proj".into(),
      file: "parallel_projects.rs".into(),
      tests: vec![sleeping_test(n, sleep)],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::default(),
    })
    .collect::<Vec<_>>();
  let total = suites.iter().map(|s| s.tests.len()).sum();
  TestPlan {
    suites,
    total_tests: total,
    shard: None,
  }
}

/// Route each project to exactly one test by grepping its unique name.
fn project(name: &str, grep: &str) -> ProjectConfig {
  ProjectConfig {
    name: name.into(),
    grep: Some(format!("> {grep}$")),
    ..Default::default()
  }
}

fn base_config(projects: Vec<ProjectConfig>, max_parallel_projects: u32) -> TestConfig {
  TestConfig {
    workers: 8,
    timeout: 30_000,
    expect_timeout: 5_000,
    quiet: true,
    reporter: vec![ReporterConfig {
      name: "none".into(),
      options: Default::default(),
    }],
    projects,
    max_parallel_projects,
    ..Default::default()
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_projects_run_concurrently() {
  let sleep = Duration::from_millis(400);
  let names = ["a", "b", "c", "d"];
  let plan = plan_with(&names, sleep);
  let projects = names.iter().map(|n| project(n, n)).collect();
  let config = base_config(projects, 0);

  let mut runner = TestRunner::new(config, CliOverrides::default());
  let start = Instant::now();
  let exit = runner.run(plan).await;
  let elapsed = start.elapsed();

  assert_eq!(exit, 0, "all projects should pass");
  // Four 400ms projects run unbounded-parallel: wall-clock must be far below
  // the 1600ms serial sum. Allow generous headroom for scheduling overhead.
  assert!(
    elapsed < sleep * 2,
    "expected ~slowest ({:?}), got {:?} — projects did not run concurrently",
    sleep,
    elapsed,
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn max_parallel_projects_one_serializes() {
  let sleep = Duration::from_millis(250);
  let names = ["a", "b", "c"];
  let plan = plan_with(&names, sleep);
  let projects = names.iter().map(|n| project(n, n)).collect();
  let config = base_config(projects, 1);

  let mut runner = TestRunner::new(config, CliOverrides::default());
  let start = Instant::now();
  let exit = runner.run(plan).await;
  let elapsed = start.elapsed();

  assert_eq!(exit, 0, "all projects should pass");
  // Cap of 1 forces strict serialization: three 250ms projects take >= ~3x.
  assert!(
    elapsed >= sleep * 2,
    "cap=1 should serialize projects; expected >= {:?}, got {:?}",
    sleep * 2,
    elapsed,
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dependency_orders_dependent_after_dependency() {
  // `setup` (long) -> `app` (short). Even with unbounded parallelism, `app`
  // must wait for `setup`, so the wall-clock is the sum, not the max.
  let setup_sleep = Duration::from_millis(500);
  let app_sleep = Duration::from_millis(200);
  let plan = TestPlan {
    suites: vec![
      TestSuite {
        name: "proj".into(),
        file: "parallel_projects.rs".into(),
        tests: vec![sleeping_test("setup_t", setup_sleep)],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: SuiteMode::default(),
      },
      TestSuite {
        name: "proj".into(),
        file: "parallel_projects.rs".into(),
        tests: vec![sleeping_test("app_t", app_sleep)],
        hooks: Hooks::default(),
        annotations: Vec::new(),
        mode: SuiteMode::default(),
      },
    ],
    total_tests: 2,
    shard: None,
  };

  let setup = project("setup", "setup_t");
  let app = ProjectConfig {
    name: "app".into(),
    grep: Some("> app_t$".into()),
    dependencies: vec!["setup".into()],
    ..Default::default()
  };
  let config = base_config(vec![setup, app], 0);

  let mut runner = TestRunner::new(config, CliOverrides::default());
  let start = Instant::now();
  let exit = runner.run(plan).await;
  let elapsed = start.elapsed();

  assert_eq!(exit, 0, "both projects should pass");
  // Sequential because of the dependency edge: must exceed setup + app.
  assert!(
    elapsed >= setup_sleep + app_sleep / 2,
    "dependent project ran before its dependency finished: {:?}",
    elapsed,
  );
}
