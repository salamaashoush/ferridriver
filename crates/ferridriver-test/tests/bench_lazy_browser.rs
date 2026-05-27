#![allow(clippy::cast_precision_loss, clippy::uninlined_format_args)]
//! Benchmark runner overhead for tests that never request browser fixtures.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferridriver_test::config::{CliOverrides, ReporterConfig, TestConfig};
use ferridriver_test::model::*;
use ferridriver_test::runner::TestRunner;

fn make_test(i: usize) -> TestCase {
  TestCase {
    id: TestId {
      file: "bench_lazy_browser.rs".into(),
      suite: Some("no_browser".into()),
      name: format!("case_{i:03}"),
      line: None,
    },
    test_fn: Arc::new(move |_| Box::pin(async move { Ok(()) })),
    fixture_requests: Vec::new(),
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
    use_options: None,
  }
}

async fn run_bench(label: &str, num_tests: usize, num_workers: u32) -> Duration {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "no_browser".into(),
      file: "bench_lazy_browser.rs".into(),
      tests: (0..num_tests).map(make_test).collect(),
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: num_tests,
    shard: None,
  };

  let config = TestConfig {
    workers: num_workers,
    timeout: 5_000,
    reporter: vec![ReporterConfig {
      name: "none".into(),
      options: std::collections::BTreeMap::new(),
    }],
    ..Default::default()
  };

  let mut runner = TestRunner::new(config, CliOverrides::default());
  let started = Instant::now();
  let exit_code = runner.run(plan).await;
  let elapsed = started.elapsed();
  assert_eq!(exit_code, 0);
  println!(
    "  {label:<28} {num_tests:>4} tests, {num_workers} workers => {elapsed:>6.0?}, {:>6.1} us/test",
    elapsed.as_micros() as f64 / num_tests as f64
  );
  elapsed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark, not for CI"]
async fn bench_no_browser_lazy_launch() {
  println!("\n=== no-browser runner benchmark ===\n");
  Box::pin(run_bench("100 no-browser", 100, 4)).await;
  Box::pin(run_bench("500 no-browser", 500, 8)).await;
  println!("\n=== end ===\n");
}
