#![allow(
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::uninlined_format_args,
  clippy::unnecessary_to_owned
)]
//! Micro-benchmark: measure individual operation costs.

use std::time::Instant;

use ferridriver::Browser;
use ferridriver::options::LaunchOptions;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn measure_operation_costs() {
  println!("\n=== Operation cost breakdown ===\n");

  // 1. Browser launch
  let start = Instant::now();
  let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
  println!("  Browser launch:        {:>6}ms", start.elapsed().as_millis());

  // 2. Context creation (10x average)
  let mut ctx_total = std::time::Duration::ZERO;
  let n = 10;
  for _ in 0..n {
    let start = Instant::now();
    let ctx = browser.new_context();
    let _page = ctx.new_page().await.unwrap();
    ctx_total += start.elapsed();
    ctx.close().await.ok();
  }
  println!(
    "  Context+Page create:   {:>6.1}ms (avg of {n})",
    ctx_total.as_millis() as f64 / n as f64
  );

  // 3. Page navigation (data URL, 10x average)
  let ctx = browser.new_context();
  let page = ctx.new_page().await.unwrap();
  let mut nav_total = std::time::Duration::ZERO;
  for i in 0..n {
    let url = format!("data:text/html,<title>T{i}</title><body>B{i}</body>");
    let start = Instant::now();
    page.goto(&url, None).await.unwrap();
    nav_total += start.elapsed();
  }
  println!(
    "  Navigate (data URL):   {:>6.1}ms (avg of {n})",
    nav_total.as_millis() as f64 / n as f64
  );

  // 4. Title read (10x average)
  let mut title_total = std::time::Duration::ZERO;
  for _ in 0..n {
    let start = Instant::now();
    let _ = page.title().await.unwrap();
    title_total += start.elapsed();
  }
  println!(
    "  Read title:            {:>6.1}ms (avg of {n})",
    title_total.as_millis() as f64 / n as f64
  );

  // 5. Locator click (10x average)
  page
    .goto(
      &"data:text/html,<button id='b' onclick=\"\">Go</button>".to_string(),
      None,
    )
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
    click_total.as_millis() as f64 / n as f64
  );

  // 6. Text content read (10x average)
  let mut text_total = std::time::Duration::ZERO;
  for _ in 0..n {
    let start = Instant::now();
    let _ = page.locator("#b").text_content().await.unwrap();
    text_total += start.elapsed();
  }
  println!(
    "  Text content:          {:>6.1}ms (avg of {n})",
    text_total.as_millis() as f64 / n as f64
  );

  // 7. Context close
  let start = Instant::now();
  ctx.close().await.ok();
  println!("  Context close:         {:>6}ms", start.elapsed().as_millis());

  // 8. Browser close
  let start = Instant::now();
  browser.close().await.ok();
  println!("  Browser close:         {:>6}ms", start.elapsed().as_millis());

  println!();
  let per_test_min = (ctx_total.as_millis() as f64 / n as f64) + (nav_total.as_millis() as f64 / n as f64);
  println!("  Minimum per-test floor (ctx+page+nav): {per_test_min:.1}ms");
  println!("\n=== end ===\n");
}
