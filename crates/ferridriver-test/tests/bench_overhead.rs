//! Micro-benchmark: measure individual operation costs.

use std::time::Instant;

use ferridriver::chromium;
use ferridriver::options::LaunchOptions;

fn avg_ms(total: std::time::Duration, n: u32) -> f64 {
  total.as_secs_f64() * 1000.0 / f64::from(n)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark, not for CI"]
async fn measure_operation_costs() {
  println!("\n=== Operation cost breakdown ===\n");

  // 1. Browser launch
  let start = Instant::now();
  let browser = chromium().launch(LaunchOptions::default()).await.unwrap();
  println!("  Browser launch:        {:>6}ms", start.elapsed().as_millis());

  // 2. Context creation (10x average)
  let mut ctx_total = std::time::Duration::ZERO;
  let n: u32 = 10;
  for _ in 0..n {
    let start = Instant::now();
    let ctx = browser.new_context().await.unwrap();
    let _page = ctx.new_page().await.unwrap();
    ctx_total += start.elapsed();
    ctx.close().await.expect("close context");
  }
  println!("  Context+Page create:   {:>6.1}ms (avg of {n})", avg_ms(ctx_total, n));

  // 3. Page navigation (data URL, 10x average)
  let ctx = browser.new_context().await.unwrap();
  let page = ctx.new_page().await.unwrap();
  let mut nav_total = std::time::Duration::ZERO;
  for i in 0..n {
    let url = format!("data:text/html,<title>T{i}</title><body>B{i}</body>");
    let start = Instant::now();
    page.goto(&url).await.unwrap();
    nav_total += start.elapsed();
  }
  println!("  Navigate (data URL):   {:>6.1}ms (avg of {n})", avg_ms(nav_total, n));

  // 4. Title read (10x average)
  let mut title_total = std::time::Duration::ZERO;
  for _ in 0..n {
    let start = Instant::now();
    let _ = page.title().await.unwrap();
    title_total += start.elapsed();
  }
  println!(
    "  Read title:            {:>6.1}ms (avg of {n})",
    avg_ms(title_total, n)
  );

  // 5. Locator click (10x average)
  page
    .goto("data:text/html,<button id='b' onclick=\"\">Go</button>")
    .await
    .unwrap();
  let mut click_total = std::time::Duration::ZERO;
  for _ in 0..n {
    let start = Instant::now();
    page.locator("#b").click().await.unwrap();
    click_total += start.elapsed();
  }
  println!(
    "  Locator click:         {:>6.1}ms (avg of {n})",
    avg_ms(click_total, n)
  );

  // 6. Text content read (10x average)
  let mut text_total = std::time::Duration::ZERO;
  for _ in 0..n {
    let start = Instant::now();
    let _ = page.locator("#b").text_content().await.unwrap();
    text_total += start.elapsed();
  }
  println!("  Text content:          {:>6.1}ms (avg of {n})", avg_ms(text_total, n));

  // 7. Context close
  let start = Instant::now();
  ctx.close().await.expect("close context");
  println!("  Context close:         {:>6}ms", start.elapsed().as_millis());

  // 8. Browser close
  let start = Instant::now();
  browser.close().await.expect("close browser");
  println!("  Browser close:         {:>6}ms", start.elapsed().as_millis());

  println!();
  let per_test_min = (avg_ms(ctx_total, n)) + (avg_ms(nav_total, n));
  println!("  Minimum per-test floor (ctx+page+nav): {per_test_min:.1}ms");
  println!("\n=== end ===\n");
}
