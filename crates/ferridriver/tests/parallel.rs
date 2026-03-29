//! Test multi-page automation.
//!
//! Note: chromiumoxide multiplexes all pages over one WebSocket connection.
//! True parallel `tokio::join!` on different pages works but CDP commands
//! are serialized through the single handler. For independent parallelism,
//! use separate Browser instances or the cdp-pipe backend.

use ferridriver::Browser;
use ferridriver::backend::BackendKind;
use ferridriver::options::*;
use std::time::Instant;

fn data_url(html: &str) -> String {
  format!(
    "data:text/html,{}",
    html
      .bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
          (b as char).to_string()
        },
        _ => format!("%{:02X}", b),
      })
      .collect::<String>()
  )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_page_automation() {
  let t0 = Instant::now();

  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch");

  // Create 3 pages with different content
  let page1 = browser.new_page().await.unwrap();
  let page2 = browser.new_page().await.unwrap();
  let page3 = browser.new_page().await.unwrap();

  let url1 = data_url("<h1>Page One</h1><input id='i' type='text'>");
  let url2 = data_url("<h1>Page Two</h1><button id='b' onclick=\"this.textContent='clicked'\">Go</button>");
  let url3 = data_url("<h1>Page Three</h1><ul><li>A</li><li>B</li><li>C</li></ul>");

  page1.goto(&url1, None).await.unwrap();
  page2.goto(&url2, None).await.unwrap();
  page3.goto(&url3, None).await.unwrap();

  // Act on each page -- each has independent state
  page1.locator("#i").fill("multi-page").await.unwrap();
  page2.locator("#b").click().await.unwrap();
  let count = page3.locator("css=li").count().await.unwrap();

  // Verify each page has independent state
  let v1 = page1.locator("#i").input_value().await.unwrap();
  let v2 = page2
    .evaluate_str("document.getElementById('b').textContent")
    .await
    .unwrap();

  assert!(v1.contains("multi-page"), "page1: {v1}");
  assert!(v2.contains("clicked"), "page2: {v2}");
  assert_eq!(count, 3, "page3 count");

  // Screenshots from different pages
  let s1 = page1.screenshot(ScreenshotOptions::default()).await.unwrap();
  let s2 = page2.screenshot(ScreenshotOptions::default()).await.unwrap();
  assert!(s1.len() > 100);
  assert!(s2.len() > 100);
  // Different pages should produce different screenshots
  assert_ne!(s1, s2, "different pages should have different screenshots");

  let elapsed = t0.elapsed();
  eprintln!("Multi-page test completed in {:.1}s", elapsed.as_secs_f64());

  std::process::exit(0);
}
