#![allow(
  clippy::too_many_lines,
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::cast_sign_loss,
  clippy::uninlined_format_args,
  clippy::unwrap_used
)]
//! Deep profiling: measure every microsecond in the test runner critical path.
//! Identifies exactly where time is spent: browser launch, context creation,
//! page creation, navigation, CDP operations, fixture pool overhead, dispatch, etc.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferridriver::Browser;
use ferridriver::options::LaunchOptions;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark, not for CI"]
async fn deep_profile() {
  println!("\n======================================================================");
  println!("  DEEP PROFILING: microsecond-level breakdown");
  println!("======================================================================\n");

  let iters = 20;

  // ── 1. Browser launch cost ──
  println!("  [1] Browser launch (parallel vs sequential)");
  let t = Instant::now();
  let b1 = Browser::launch(LaunchOptions::default()).await.unwrap();
  println!(
    "      Single browser launch:   {:>7.1}ms",
    t.elapsed().as_secs_f64() * 1000.0
  );

  let t = Instant::now();
  let (b2, b3, b4) = tokio::join!(
    Browser::launch(LaunchOptions::default()),
    Browser::launch(LaunchOptions::default()),
    Browser::launch(LaunchOptions::default()),
  );
  let b2 = b2.unwrap();
  let b3 = b3.unwrap();
  let b4 = b4.unwrap();
  println!(
    "      3 browsers parallel:     {:>7.1}ms (amortized: {:.1}ms each)",
    t.elapsed().as_secs_f64() * 1000.0,
    t.elapsed().as_secs_f64() * 1000.0 / 3.0
  );
  b2.close().await.ok();
  b3.close().await.ok();
  b4.close().await.ok();
  println!();

  // ── 2. Context creation breakdown ──
  println!("  [2] Context + Page creation ({iters} iterations)");
  let mut ctx_times = Vec::new();
  let mut page_times = Vec::new();
  let mut total_times = Vec::new();
  for _ in 0..iters {
    let t0 = Instant::now();
    let ctx = b1.new_context();
    let t1 = Instant::now();
    let _page = ctx.new_page().await.unwrap();
    let t2 = Instant::now();
    ctx_times.push(t1.duration_since(t0));
    page_times.push(t2.duration_since(t1));
    total_times.push(t2.duration_since(t0));
    ctx.close().await.ok();
  }
  let ctx_avg = ctx_times.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / iters as f64;
  let page_avg = page_times.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / iters as f64;
  let total_avg = total_times.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / iters as f64;
  let total_p50 = percentile(&total_times, 50);
  let total_p95 = percentile(&total_times, 95);
  println!("      new_context() only:      {:>7.2}ms avg", ctx_avg);
  println!("      new_page() (CDP calls):  {:>7.2}ms avg", page_avg);
  println!(
    "      ctx+page combined:       {:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
    total_avg, total_p50, total_p95
  );
  println!();

  // ── 3. Navigation cost ──
  println!("  [3] Navigation cost ({iters} iterations)");
  let ctx = b1.new_context();
  let page = ctx.new_page().await.unwrap();
  let mut nav_times = Vec::new();
  for i in 0..iters {
    let url = data_url(&format!("<title>T{i}</title><body>B{i}</body>"));
    let t = Instant::now();
    page.goto(&url, None).await.unwrap();
    nav_times.push(t.elapsed());
  }
  let nav_avg = nav_times.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / iters as f64;
  let nav_p50 = percentile(&nav_times, 50);
  let nav_p95 = percentile(&nav_times, 95);
  println!(
    "      data URL goto:           {:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
    nav_avg, nav_p50, nav_p95
  );

  // ── 4. CDP operation costs ──
  println!();
  println!("  [4] Individual CDP operations ({iters} iterations)");
  let mut title_times = Vec::new();
  let mut click_times = Vec::new();
  let mut text_times = Vec::new();
  let mut eval_times = Vec::new();

  page.goto(&data_url("<button id='b'>Go</button>"), None).await.unwrap();
  for _ in 0..iters {
    let t = Instant::now();
    page.title().await.unwrap();
    title_times.push(t.elapsed());
    let t = Instant::now();
    page.locator("#b").click().await.unwrap();
    click_times.push(t.elapsed());
    let t = Instant::now();
    page.locator("#b").text_content().await.unwrap();
    text_times.push(t.elapsed());
    let t = Instant::now();
    page.evaluate("1+1").await.unwrap();
    eval_times.push(t.elapsed());
  }
  println!("      title():                 {:>7.2}ms avg", avg_ms(&title_times));
  println!("      locator.click():         {:>7.2}ms avg", avg_ms(&click_times));
  println!("      locator.text_content():  {:>7.2}ms avg", avg_ms(&text_times));
  println!("      evaluate('1+1'):         {:>7.2}ms avg", avg_ms(&eval_times));

  ctx.close().await.ok();
  println!();

  // ── 5. Context close cost ──
  println!("  [5] Context close cost ({iters} iterations)");
  let mut close_times = Vec::new();
  for _ in 0..iters {
    let ctx = b1.new_context();
    let _page = ctx.new_page().await.unwrap();
    let t = Instant::now();
    ctx.close().await.ok();
    close_times.push(t.elapsed());
  }
  println!(
    "      context.close():         {:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
    avg_ms(&close_times),
    percentile(&close_times, 50),
    percentile(&close_times, 95)
  );
  println!();

  // ── 6. Full test cycle (what the runner does per test) ──
  println!("  [6] Full test cycle: ctx+page+nav+click+text+close ({iters} iterations)");
  let mut cycle_times = Vec::new();
  for i in 0..iters {
    let t = Instant::now();
    let ctx = b1.new_context();
    let page = ctx.new_page().await.unwrap();
    let url = data_url(&format!(
      "<button id='b' onclick=\"this.textContent='done'\">Click {i}</button>"
    ));
    page.goto(&url, None).await.unwrap();
    page.locator("#b").click().await.unwrap();
    let _ = page.locator("#b").text_content().await.unwrap();
    ctx.close().await.ok();
    cycle_times.push(t.elapsed());
  }
  let cycle_avg = avg_ms(&cycle_times);
  let cycle_p50 = percentile(&cycle_times, 50);
  let cycle_p95 = percentile(&cycle_times, 95);
  println!(
    "      Full cycle:              {:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
    cycle_avg, cycle_p50, cycle_p95
  );
  println!();

  // ── 7. Dispatch overhead (MPMC channel) ──
  println!("  [7] Dispatch channel overhead ({iters}k iterations)");
  {
    let (tx, rx) = async_channel::unbounded::<u32>();
    let n = iters * 1000;
    let t = Instant::now();
    for i in 0..n as u32 {
      let _ = tx.send(i).await;
    }
    for _ in 0..n {
      let _ = rx.recv().await;
    }
    let ch_ns = t.elapsed().as_nanos() as f64 / n as f64;
    println!("      send+recv per item:      {:>7.0}ns", ch_ns);
  }
  println!();

  // ── 8. Runner overhead measurement ──
  println!("  [8] Runner framework overhead (1 no-op test, 1 worker)");
  let noop_test = TestCase {
    id: TestId {
      file: "bench".into(),
      suite: None,
      name: "noop".into(),
      line: None,
    },
    test_fn: Arc::new(|_pool| Box::pin(async { Ok(()) })),
    fixture_requests: vec![],
    annotations: Vec::new(),
    timeout: Some(Duration::from_secs(5)),
    retries: None,
    expected_status: ExpectedStatus::Pass,
      use_options: None,
  };
  let plan = TestPlan {
    suites: vec![TestSuite {
      name: "noop".into(),
      file: "bench".into(),
      tests: vec![noop_test],
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: SuiteMode::Parallel,
    }],
    total_tests: 1,
    shard: None,
  };
  let config = TestConfig {
    workers: 1,
    timeout: 5000,
    reporter: vec![],
    ..Default::default()
  };
  let t = Instant::now();
  let mut runner = TestRunner::new(config, CliOverrides::default());
  let _ = runner.run(plan).await;
  println!(
    "      1 no-op test, 1 worker:  {:>7.1}ms (= browser launch + dispatch overhead)",
    t.elapsed().as_secs_f64() * 1000.0
  );
  println!();

  // ── Summary ──
  println!("  SUMMARY: Per-test cost breakdown");
  println!("  ────────────────────────────────");
  println!(
    "      context + page creation: {:>6.2}ms  (irreducible CDP cost)",
    total_avg
  );
  println!("      navigation (data URL):   {:>6.2}ms", nav_avg);
  println!(
    "      click + text_content:    {:>6.2}ms",
    avg_ms(&click_times) + avg_ms(&text_times)
  );
  println!("      context close:           {:>6.2}ms", avg_ms(&close_times));
  println!("      ─────────────────────────────────");
  println!("      Total per-test (serial): {:>6.2}ms", cycle_avg);
  println!(
    "      Theoretical 50 tests / 4 workers: {:.0}ms",
    cycle_avg * 50.0 / 4.0
  );
  println!();

  b1.close().await.ok();
  println!("======================================================================\n");
}

fn avg_ms(times: &[Duration]) -> f64 {
  times.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / times.len() as f64
}

fn percentile(times: &[Duration], pct: usize) -> f64 {
  let mut sorted: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
  sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
  let idx = (sorted.len() * pct / 100).min(sorted.len() - 1);
  sorted[idx]
}
