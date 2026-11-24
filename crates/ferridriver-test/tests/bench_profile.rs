//! Deep profiling: measure every microsecond in the test runner critical path.
//! Identifies exactly where time is spent: browser launch, context creation,
//! page creation, navigation, CDP operations, fixture pool overhead, dispatch, etc.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferridriver::chromium;
use ferridriver::options::LaunchOptions;
use ferridriver_test::config::{CliOverrides, ReporterConfig, TestConfig};
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
async fn deep_profile() -> anyhow::Result<()> {
  ferridriver_test::logging::init(1);
  println!("\n======================================================================");
  println!("  DEEP PROFILING: microsecond-level breakdown");
  println!("======================================================================\n");

  let iters = 20u32;

  // ── 1. Browser launch cost ──
  println!("  [1] Browser launch (parallel vs sequential)");
  let t = Instant::now();
  let b1 = chromium().launch(LaunchOptions::default()).await?;
  println!(
    "      Single browser launch:   {:>7.1}ms",
    t.elapsed().as_secs_f64() * 1000.0
  );

  let t = Instant::now();
  let (b2, b3, b4) = tokio::join!(
    chromium().launch(LaunchOptions::default()),
    chromium().launch(LaunchOptions::default()),
    chromium().launch(LaunchOptions::default()),
  );
  let (b2, b3, b4) = (b2?, b3?, b4?);
  println!(
    "      3 browsers parallel:     {:>7.1}ms (amortized: {:.1}ms each)",
    t.elapsed().as_secs_f64() * 1000.0,
    t.elapsed().as_secs_f64() * 1000.0 / 3.0
  );
  b2.close().await?;
  b3.close().await?;
  b4.close().await?;
  println!();

  let create = Box::pin(profile_context_creation(&b1, iters)).await?;
  println!("{}", create.lines.join("\n"));
  let ops = Box::pin(profile_page_operations(&b1, iters)).await?;
  println!("{}", ops.lines.join("\n"));
  let close = Box::pin(profile_context_close(&b1, iters)).await?;
  println!("{}", close.lines.join("\n"));
  let cycle = Box::pin(profile_test_cycle(&b1, iters)).await?;
  println!("{}", cycle.lines.join("\n"));
  println!("{}", Box::pin(profile_dispatch(iters)).await.join("\n"));
  println!("{}", Box::pin(profile_runner_overhead()).await.join("\n"));

  // ── Summary ──
  println!("  SUMMARY: Per-test cost breakdown");
  println!("  ────────────────────────────────");
  println!(
    "      context + page creation: {:>6.2}ms  (irreducible CDP cost)",
    create.avg_ms
  );
  println!("      navigation (data URL):   {:>6.2}ms", ops.avg_ms);
  println!("      click + text_content:    {:>6.2}ms", ops.click_and_text_ms);
  println!("      context close:           {:>6.2}ms", close.avg_ms);
  println!("      ─────────────────────────────────");
  println!("      Total per-test (serial): {:>6.2}ms", cycle.avg_ms);
  println!(
    "      Theoretical 50 tests / 4 workers: {:.0}ms",
    cycle.avg_ms * 50.0 / 4.0
  );
  println!();

  b1.close().await?;
  println!("======================================================================\n");
  Ok(())
}

/// One profiled section: the lines that report it and the figure the
/// summary takes from it.
struct Section {
  lines: Vec<String>,
  avg_ms: f64,
  click_and_text_ms: f64,
}

impl Section {
  fn new(lines: Vec<String>, avg_ms: f64) -> Self {
    Self {
      lines,
      avg_ms,
      click_and_text_ms: 0.0,
    }
  }
}

// ── 2. Context creation breakdown ──
async fn profile_context_creation(b1: &ferridriver::Browser, iters: u32) -> anyhow::Result<Section> {
  let mut ctx_times = Vec::new();
  let mut page_times = Vec::new();
  let mut total_times = Vec::new();
  for _ in 0..iters {
    let t0 = Instant::now();
    let ctx = b1.new_context().await?;
    let t1 = Instant::now();
    let _page = ctx.new_page().await?;
    let t2 = Instant::now();
    ctx_times.push(t1.duration_since(t0));
    page_times.push(t2.duration_since(t1));
    total_times.push(t2.duration_since(t0));
    ctx.close().await?;
  }
  let total_avg = avg_ms(&total_times);
  let lines = vec![
    format!("  [2] Context + Page creation ({iters} iterations)"),
    format!("      new_context() only:      {:>7.2}ms avg", avg_ms(&ctx_times)),
    format!("      new_page() (CDP calls):  {:>7.2}ms avg", avg_ms(&page_times)),
    format!(
      "      ctx+page combined:       {total_avg:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
      percentile(&total_times, 50),
      percentile(&total_times, 95)
    ),
    String::new(),
  ];
  Ok(Section::new(lines, total_avg))
}

// ── 3. Navigation cost, 4. CDP operation costs ──
async fn profile_page_operations(b1: &ferridriver::Browser, iters: u32) -> anyhow::Result<Section> {
  let ctx = b1.new_context().await?;
  let page = ctx.new_page().await?;
  let mut nav_times = Vec::new();
  for i in 0..iters {
    let url = data_url(&format!("<title>T{i}</title><body>B{i}</body>"));
    let t = Instant::now();
    page.goto(&url).await?;
    nav_times.push(t.elapsed());
  }
  let nav_avg = avg_ms(&nav_times);

  let mut title_times = Vec::new();
  let mut click_times = Vec::new();
  let mut text_times = Vec::new();
  let mut eval_times = Vec::new();
  page.goto(&data_url("<button id='b'>Go</button>")).await?;
  for _ in 0..iters {
    let t = Instant::now();
    page.title().await?;
    title_times.push(t.elapsed());
    let t = Instant::now();
    page.locator("#b").click().await?;
    click_times.push(t.elapsed());
    let t = Instant::now();
    page.locator("#b").text_content().await?;
    text_times.push(t.elapsed());
    let t = Instant::now();
    page
      .evaluate("1+1", ferridriver::protocol::SerializedArgument::default(), None)
      .await?;
    eval_times.push(t.elapsed());
  }
  ctx.close().await?;
  let lines = vec![
    format!("  [3] Navigation cost ({iters} iterations)"),
    format!(
      "      data URL goto:           {nav_avg:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
      percentile(&nav_times, 50),
      percentile(&nav_times, 95)
    ),
    String::new(),
    format!("  [4] Individual CDP operations ({iters} iterations)"),
    format!("      title():                 {:>7.2}ms avg", avg_ms(&title_times)),
    format!("      locator.click():         {:>7.2}ms avg", avg_ms(&click_times)),
    format!("      locator.text_content():  {:>7.2}ms avg", avg_ms(&text_times)),
    format!("      evaluate('1+1'):         {:>7.2}ms avg", avg_ms(&eval_times)),
    String::new(),
  ];
  Ok(Section {
    click_and_text_ms: avg_ms(&click_times) + avg_ms(&text_times),
    ..Section::new(lines, nav_avg)
  })
}

// ── 5. Context close cost ──
async fn profile_context_close(b1: &ferridriver::Browser, iters: u32) -> anyhow::Result<Section> {
  let mut close_times = Vec::new();
  for _ in 0..iters {
    let ctx = b1.new_context().await?;
    let _page = ctx.new_page().await?;
    let t = Instant::now();
    ctx.close().await?;
    close_times.push(t.elapsed());
  }
  let lines = vec![
    format!("  [5] Context close cost ({iters} iterations)"),
    format!(
      "      context.close():         {:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
      avg_ms(&close_times),
      percentile(&close_times, 50),
      percentile(&close_times, 95)
    ),
    String::new(),
  ];
  Ok(Section::new(lines, avg_ms(&close_times)))
}

// ── 6. Full test cycle (what the runner does per test) ──
async fn profile_test_cycle(b1: &ferridriver::Browser, iters: u32) -> anyhow::Result<Section> {
  let mut cycle_times = Vec::new();
  for i in 0..iters {
    let t = Instant::now();
    let ctx = b1.new_context().await?;
    let page = ctx.new_page().await?;
    let url = data_url(&format!(
      "<button id='b' onclick=\"this.textContent='done'\">Click {i}</button>"
    ));
    page.goto(&url).await?;
    page.locator("#b").click().await?;
    page.locator("#b").text_content().await?;
    ctx.close().await?;
    cycle_times.push(t.elapsed());
  }
  let cycle_avg = avg_ms(&cycle_times);
  let lines = vec![
    format!("  [6] Full test cycle: ctx+page+nav+click+text+close ({iters} iterations)"),
    format!(
      "      Full cycle:              {cycle_avg:>7.2}ms avg, p50={:.2}ms, p95={:.2}ms",
      percentile(&cycle_times, 50),
      percentile(&cycle_times, 95)
    ),
    String::new(),
  ];
  Ok(Section::new(lines, cycle_avg))
}

// ── 7. Dispatch overhead (MPMC channel) ──
async fn profile_dispatch(iters: u32) -> Vec<String> {
  let (tx, rx) = async_channel::unbounded::<u32>();
  let n = iters * 1000;
  let t = Instant::now();
  for i in 0..n {
    if tx.send(i).await.is_err() {
      break;
    }
  }
  for _ in 0..n {
    if rx.recv().await.is_err() {
      break;
    }
  }
  let ch_ns = t.elapsed().as_secs_f64() * 1_000_000_000.0 / f64::from(n);
  vec![
    format!("  [7] Dispatch channel overhead ({iters}k iterations)"),
    format!("      send+recv per item:      {ch_ns:>7.0}ns"),
    String::new(),
  ]
}

// ── 8. Runner overhead measurement ──
async fn profile_runner_overhead() -> Vec<String> {
  let noop_test = TestCase {
    metadata: None,
    id: TestId {
      root_path: None,
      repeat_each_index: 0,
      file: "bench".into(),
      suite: None,
      name: "noop".into(),
      line: None,
      column: None,
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
    repetitions_expanded: false,
    suites: vec![TestSuite {
      serial_scope: None,
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
    workers: Some(1),
    timeout: 5000,
    reporter: vec![ReporterConfig {
      name: "none".into(),
      options: std::collections::BTreeMap::new(),
    }],
    ..Default::default()
  };
  let t = Instant::now();
  let mut runner = TestRunner::new(config, CliOverrides::default());
  let exit_code = runner.run(plan).await;
  vec![
    "  [8] Runner framework overhead (1 no-op test, 1 worker)".to_string(),
    format!(
      "      1 no-op test, 1 worker:  {:>7.1}ms (= browser launch + dispatch overhead, exit {exit_code})",
      t.elapsed().as_secs_f64() * 1000.0
    ),
    String::new(),
  ]
}

fn avg_ms(times: &[Duration]) -> f64 {
  times.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>()
    / f64::from(u32::try_from(times.len()).unwrap_or(u32::MAX))
}

fn percentile(times: &[Duration], pct: usize) -> f64 {
  let mut sorted: Vec<f64> = times.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
  sorted.sort_by(f64::total_cmp);
  let idx = (sorted.len() * pct / 100).min(sorted.len().saturating_sub(1));
  sorted.get(idx).copied().unwrap_or_default()
}
