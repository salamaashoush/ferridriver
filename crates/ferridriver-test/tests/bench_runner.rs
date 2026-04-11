#![allow(
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::uninlined_format_args,
  clippy::implicit_clone
)]
//! Performance benchmark: measures ferridriver-test runner overhead and parallelism.
//! Compare against Playwright Test for the same 50-test workload.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferridriver_test::config::{CliOverrides, TestConfig};
use ferridriver_test::model::*;
use ferridriver_test::runner::TestRunner;

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

fn make_nav_test(i: usize) -> TestCase {
  TestCase {
    id: TestId {
      file: "bench.rs".into(),
      suite: Some("nav".into()),
      name: format!("nav_{i:03}"),
      line: None,
    },
    test_fn: Arc::new(move |pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(|e| TestFailure {
          message: e,
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let html = format!("<title>Test {i}</title><body><h1>Page {i}</h1></body>");
        page.goto(&data_url(&html), None).await.map_err(|e| TestFailure {
          message: e.to_string(),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let title = page.title().await.map_err(|e| TestFailure {
          message: e.to_string(),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        assert!(title.contains(&format!("Test {i}")));
        Ok(())
      })
    }),
    fixture_requests: vec!["page".into()],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(10)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
      use_options: None,
  }
}

fn make_interaction_test(i: usize) -> TestCase {
  TestCase {
    id: TestId {
      file: "bench.rs".into(),
      suite: Some("click".into()),
      name: format!("interact_{i:03}"),
      line: None,
    },
    test_fn: Arc::new(move |pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(|e| TestFailure {
          message: e,
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let html = format!("<button id='btn' onclick=\"this.textContent='done {i}'\">Click {i}</button>");
        page.goto(&data_url(&html), None).await.map_err(|e| TestFailure {
          message: e.to_string(),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        page.locator("#btn").click().await.map_err(|e| TestFailure {
          message: e.to_string(),
          stack: None,
          diff: None,
          screenshot: None,
        })?;
        let text = page
          .locator("#btn")
          .text_content()
          .await
          .map_err(|e| TestFailure {
            message: e.to_string(),
            stack: None,
            diff: None,
            screenshot: None,
          })?
          .unwrap_or_default();
        assert!(text.contains(&format!("done {i}")));
        Ok(())
      })
    }),
    fixture_requests: vec!["page".into()],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(10)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
      use_options: None,
  }
}

fn make_tests(n: usize) -> Vec<TestCase> {
  (0..n)
    .map(|i| {
      if i % 2 == 0 {
        make_nav_test(i)
      } else {
        make_interaction_test(i)
      }
    })
    .collect()
}

async fn run_bench(label: &str, num_tests: usize, num_workers: u32) -> Duration {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "bench".into(),
      file: "bench.rs".into(),
      tests: make_tests(num_tests),
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: num_tests,
    shard: None,
  };

  let config = TestConfig {
    workers: num_workers,
    timeout: 10_000,
    reporter: vec![],
    ..Default::default()
  };

  let mut runner = TestRunner::new(config, CliOverrides::default());

  let start = Instant::now();
  let exit_code = runner.run(plan).await;
  let elapsed = start.elapsed();

  let per_test = elapsed.as_millis() as f64 / num_tests as f64;
  let tests_per_sec = num_tests as f64 / elapsed.as_secs_f64();

  println!(
    "  {label:<30} {num_tests:>3} tests, {num_workers} workers => {elapsed:>6.0?} total, {per_test:>5.1}ms/test, {tests_per_sec:>5.1} tests/sec",
  );

  assert_eq!(exit_code, 0, "{label}: all tests should pass");
  elapsed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark, not for CI"]
async fn bench_parallel_scaling() {
  println!("\n============================================================");
  println!("  ferridriver-test performance benchmark");
  println!("============================================================\n");

  // Warm up: single test to launch browser.
  run_bench("warmup (browser launch)", 1, 1).await;
  println!();

  // Worker scaling: 20 tests.
  let t1 = run_bench("20 tests, 1 worker", 20, 1).await;
  let t2 = run_bench("20 tests, 2 workers", 20, 2).await;
  let t4 = run_bench("20 tests, 4 workers", 20, 4).await;

  println!();
  println!("  Speedup 1→2: {:.2}x", t1.as_secs_f64() / t2.as_secs_f64());
  println!("  Speedup 1→4: {:.2}x", t1.as_secs_f64() / t4.as_secs_f64());
  println!();

  // Throughput: 50 tests (matches Playwright bench).
  let t50_4 = run_bench("50 tests, 4 workers", 50, 4).await;
  let t50_6 = run_bench("50 tests, 6 workers", 50, 6).await;

  println!();
  println!("  Playwright comparison (50 tests): ~2200ms (self-reported)");
  println!("  ferridriver-test (50 tests, 4w): {}ms", t50_4.as_millis());
  println!("  ferridriver-test (50 tests, 6w): {}ms", t50_6.as_millis());
  println!(
    "  Speedup vs Playwright (4w): {:.1}x",
    2200.0 / t50_4.as_millis() as f64
  );
  println!(
    "  Speedup vs Playwright (6w): {:.1}x",
    2200.0 / t50_6.as_millis() as f64
  );
  println!();

  // Large scale: 100 tests.
  run_bench("100 tests, 4 workers", 100, 4).await;
  run_bench("100 tests, 6 workers", 100, 6).await;

  println!("\n============================================================\n");
}
