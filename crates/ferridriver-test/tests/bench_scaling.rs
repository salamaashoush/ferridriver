#![allow(
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::too_many_lines,
  clippy::uninlined_format_args
)]
//! Diagnose why worker scaling is sub-linear.
//! Measure: browser launch overlap, per-worker test time, idle time.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ferridriver::Browser;
use ferridriver::options::LaunchOptions;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "benchmark, not for CI"]
async fn diagnose_scaling() {
  println!("\n=== Scaling Diagnosis ===\n");

  // 1. How long does parallel browser launch take vs sequential?
  println!("  [1] Browser launch scaling");
  for n in [1, 2, 4, 6] {
    let t = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..n {
      handles.push(tokio::spawn(Browser::launch(LaunchOptions::default())));
    }
    let mut browsers = Vec::new();
    for h in handles {
      browsers.push(h.await.unwrap().unwrap());
    }
    let launch_ms = t.elapsed().as_millis();
    // Close all
    for b in &browsers {
      b.close(None).await.ok();
    }
    println!(
      "      {n} browser(s) parallel: {launch_ms}ms (amortized: {:.1}ms each)",
      launch_ms as f64 / n as f64
    );
  }
  println!();

  // 2. Does per-test time degrade with more concurrent Chrome processes?
  println!("  [2] Per-test time with N Chrome processes running");
  // Launch N browsers and measure ctx+page+nav+close on one of them
  // while others are idle (just existing).
  for n in [1, 2, 4, 6] {
    let mut browsers = Vec::new();
    for _ in 0..n {
      browsers.push(Browser::launch(LaunchOptions::default()).await.unwrap());
    }

    // Measure 10 test cycles on the first browser.
    let browser = &browsers[0];
    let iters = 10;
    let t = Instant::now();
    for i in 0..iters {
      let ctx = browser.new_context();
      let page = ctx.new_page().await.unwrap();
      let url =
        format!("data:text/html,<title>T{i}</title><button id='b' onclick=\"this.textContent='d'\">Go</button>");
      page.goto(&url, None).await.unwrap();
      page.locator("#b").click().await.unwrap();
      let _ = page.locator("#b").text_content().await.unwrap();
      ctx.close().await.ok();
    }
    let per_test = t.elapsed().as_millis() as f64 / iters as f64;
    println!("      {n} Chrome process(es): {per_test:.1}ms/test");

    for b in &browsers {
      b.close(None).await.ok();
    }
  }
  println!();

  // 3. Measure per-test time with N browsers ALL running tests concurrently.
  println!("  [3] Per-test time with N browsers ALL running tests concurrently");
  for n in [1u32, 2, 4, 6] {
    let mut browsers: Vec<Arc<Browser>> = Vec::new();
    for _ in 0..n {
      browsers.push(Arc::new(Browser::launch(LaunchOptions::default()).await.unwrap()));
    }

    let total_tests = 20u32;
    let per_worker = total_tests / n;
    let test_counter = Arc::new(AtomicU64::new(0));
    let wall_start = Instant::now();

    let mut handles = Vec::new();
    for (wid, browser) in browsers.iter().enumerate() {
      let b = Arc::clone(browser);
      let counter = Arc::clone(&test_counter);
      handles.push(tokio::spawn(async move {
        let mut times = Vec::new();
        for _ in 0..per_worker {
          let i = counter.fetch_add(1, Ordering::Relaxed);
          let t = Instant::now();
          let ctx = b.new_context();
          let page = ctx.new_page().await.unwrap();
          let url =
            format!("data:text/html,<title>T{i}</title><button id='b' onclick=\"this.textContent='d'\">Go</button>");
          page.goto(&url, None).await.unwrap();
          page.locator("#b").click().await.unwrap();
          let _ = page.locator("#b").text_content().await.unwrap();
          ctx.close().await.ok();
          times.push(t.elapsed());
        }
        let avg = times.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / times.len() as f64;
        (wid, avg)
      }));
    }

    let mut worker_avgs = Vec::new();
    for h in handles {
      let (wid, avg) = h.await.unwrap();
      worker_avgs.push((wid, avg));
    }
    let wall_ms = wall_start.elapsed().as_millis();
    let overall_avg: f64 = worker_avgs.iter().map(|(_, a)| a).sum::<f64>() / worker_avgs.len() as f64;
    let throughput = total_tests as f64 / (wall_ms as f64 / 1000.0);

    println!(
      "      {n} workers × {per_worker} tests: wall={wall_ms}ms, avg={overall_avg:.1}ms/test, {throughput:.1} tests/sec"
    );

    for b in &browsers {
      b.close(None).await.ok();
    }
  }
  println!();

  // 4. Check if tokio runtime thread count matters.
  println!("  [4] Are we CPU-bound or I/O-bound?");
  {
    let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
    let iters = 10;

    // Measure with a CPU-heavy background task vs without.
    let t = Instant::now();
    for i in 0..iters {
      let ctx = browser.new_context();
      let page = ctx.new_page().await.unwrap();
      page
        .goto(&format!("data:text/html,<title>T{i}</title>"), None)
        .await
        .unwrap();
      ctx.close().await.ok();
    }
    let baseline = t.elapsed().as_millis() as f64 / iters as f64;

    // Now with CPU load on tokio threads.
    let cpu_tasks: Vec<_> = (0..4)
      .map(|_| {
        tokio::spawn(async {
          loop {
            tokio::task::yield_now().await;
            // Busy loop
            let mut x = 0u64;
            for i in 0..10000 {
              x = x.wrapping_add(i);
            }
            std::hint::black_box(x);
          }
        })
      })
      .collect();

    let t = Instant::now();
    for i in 0..iters {
      let ctx = browser.new_context();
      let page = ctx.new_page().await.unwrap();
      page
        .goto(&format!("data:text/html,<title>T{i}</title>"), None)
        .await
        .unwrap();
      ctx.close().await.ok();
    }
    let with_load = t.elapsed().as_millis() as f64 / iters as f64;

    for task in cpu_tasks {
      task.abort();
    }
    browser.close(None).await.ok();

    println!("      Without CPU load: {baseline:.1}ms/test");
    println!("      With CPU load:    {with_load:.1}ms/test");
    println!(
      "      Degradation:      {:.1}%",
      (with_load - baseline) / baseline * 100.0
    );
  }

  println!("\n=== end ===\n");
}
