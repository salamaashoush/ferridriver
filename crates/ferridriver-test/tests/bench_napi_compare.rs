#![allow(
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::uninlined_format_args,
  clippy::implicit_clone
)]
//! Apples-to-apples bench against `bench/fd-bench/bench_compare.spec.ts`.
//!
//! Uses the production [`ferridriver_test::runner::TestRunner`] — same
//! per-worker browser launch, same dispatcher, same per-test context
//! creation — but registers test cases directly in Rust. NO NAPI, NO
//! Bun, NO Node, NO TS in the loop. Lets us pin down whether the 8w
//! shell anomaly observed in the JS-runner bench is NAPI-side or
//! core-side.
//!
//! Test split mirrors the TS bench exactly:
//!   - 33 nav   (`page.goto + assert title`)
//!   - 33 click (`page.goto + locator.click + poll text`)
//!   - 34 eval  (`page.goto + page.evaluate('document.getElementById...')`)
//!
//! Worker matrix: 1, 2, 4, 8.
//!
//! Run with:
//!   `cargo test -p ferridriver-test --release --test bench_napi_compare -- --ignored --nocapture`

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferridriver_test::config::{CliOverrides, TestConfig};
use ferridriver_test::model::*;
use ferridriver_test::runner::TestRunner;

fn data_url(html: &str) -> String {
  let mut out = String::with_capacity(html.len() * 3);
  out.push_str("data:text/html,");
  for b in html.bytes() {
    match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
      _ => {
        let _ = write!(out, "%{b:02X}");
      },
    }
  }
  out
}

fn fail(msg: impl Into<String>) -> TestFailure {
  TestFailure {
    message: msg.into(),
    stack: None,
    diff: None,
    screenshot: None,
  }
}

fn make_nav_test(i: usize) -> TestCase {
  TestCase {
    id: TestId {
      file: "bench_napi_compare.rs".into(),
      suite: Some("nav".into()),
      name: format!("nav_{i:03}"),
      line: None,
    },
    test_fn: Arc::new(move |pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(fail)?;
        let want = format!("Test {i}");
        let url = data_url(&format!("<title>{want}</title><body><h1>Page {i}</h1></body>"));
        page.goto(&url, None).await.map_err(|e| fail(e.to_string()))?;
        let got = page.title().await.map_err(|e| fail(e.to_string()))?;
        if got != want {
          return Err(fail(format!("nav: got {got:?} want {want:?}")));
        }
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

fn make_click_test(i: usize) -> TestCase {
  TestCase {
    id: TestId {
      file: "bench_napi_compare.rs".into(),
      suite: Some("click".into()),
      name: format!("click_{i:03}"),
      line: None,
    },
    test_fn: Arc::new(move |pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(fail)?;
        let want = format!("done {i}");
        let url = data_url(&format!(
          "<button id='btn' onclick=\"this.textContent='{want}'\">Click {i}</button>"
        ));
        page.goto(&url, None).await.map_err(|e| fail(e.to_string()))?;
        page
          .locator("#btn", None)
          .click(None)
          .await
          .map_err(|e| fail(e.to_string()))?;
        // Spin-poll text — same shape as `expect(...).toHaveText` in TS.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
          let txt = page
            .locator("#btn", None)
            .text_content()
            .await
            .map_err(|e| fail(e.to_string()))?
            .unwrap_or_default();
          if txt == want {
            return Ok(());
          }
          if Instant::now() > deadline {
            return Err(fail(format!("click: text never reached {want:?}")));
          }
          tokio::time::sleep(Duration::from_millis(20)).await;
        }
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

fn make_eval_test(i: usize) -> TestCase {
  TestCase {
    id: TestId {
      file: "bench_napi_compare.rs".into(),
      suite: Some("eval".into()),
      name: format!("eval_{i:03}"),
      line: None,
    },
    test_fn: Arc::new(move |pool| {
      Box::pin(async move {
        let page: Arc<ferridriver::Page> = pool.get("page").await.map_err(fail)?;
        let want = format!("{i}");
        let url = data_url(&format!("<title>Eval {i}</title><div id='out'>{i}</div>"));
        page.goto(&url, None).await.map_err(|e| fail(e.to_string()))?;
        let v = page
          .evaluate(
            "document.getElementById('out')?.textContent",
            ferridriver::protocol::SerializedArgument::default(),
            None,
          )
          .await
          .map_err(|e| fail(e.to_string()))?;
        let got = v.as_str().unwrap_or("").to_string();
        if got != want {
          return Err(fail(format!("eval: got {got:?} want {want:?}")));
        }
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

/// Mirrors the TS bench's 33 nav / 33 click / 34 eval split deterministically.
fn make_tests() -> Vec<TestCase> {
  (0..100)
    .map(|i| match i % 3 {
      0 => make_nav_test(i),
      1 => make_click_test(i),
      _ => make_eval_test(i),
    })
    .collect()
}

async fn run_one(workers: u32) -> Duration {
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "bench_napi_compare".into(),
      file: "bench_napi_compare.rs".into(),
      tests: make_tests(),
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: 100,
    shard: None,
  };

  let config = TestConfig {
    workers,
    timeout: 10_000,
    reporter: vec![],
    ..Default::default()
  };

  let mut runner = TestRunner::new(config, CliOverrides::default());
  let start = Instant::now();
  let exit_code = runner.run(plan).await;
  let elapsed = start.elapsed();
  assert_eq!(exit_code, 0, "all 100 tests should pass at workers={workers}");
  elapsed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark; run with --ignored --nocapture"]
async fn bench_pure_rust_runner() {
  println!("\n=== ferridriver pure-Rust runner — 100 tests, single run ===\n");
  println!("  (no NAPI, no Bun, no Node, no TS — same TestRunner as production)\n");

  for w in [1u32, 2, 4, 8] {
    let elapsed = Box::pin(run_one(w)).await;
    println!(
      "  workers={w:<2}  {:>6.0?} total  {:>5.1}ms/test",
      elapsed,
      elapsed.as_millis() as f64 / 100.0
    );
  }
  println!();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark; run with --ignored --nocapture"]
async fn bench_pure_rust_runner_3runs() {
  println!("\n=== ferridriver pure-Rust runner — 100 tests, 3 runs avg ===\n");
  for w in [1u32, 2, 4, 8] {
    let mut total = Duration::default();
    for _ in 0..3 {
      total += Box::pin(run_one(w)).await;
    }
    let avg = total / 3;
    println!(
      "  workers={w:<2}  avg {:>6.0?}  {:>5.1}ms/test",
      avg,
      avg.as_millis() as f64 / 100.0
    );
  }
  println!();
}
