#![allow(clippy::cast_lossless, clippy::unwrap_used)]
//! Measure futex calls per operation.

use ferridriver::Browser;
use ferridriver::options::LaunchOptions;
use std::time::Instant;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  let browser = Browser::launch(LaunchOptions::default()).await.unwrap();
  eprintln!("=== BROWSER LAUNCHED, starting test cycles ===");

  // Marker so strace -e write can see it
  let _ = std::io::Write::write_all(&mut std::io::stderr(), b"MARKER_START\n");

  for i in 0..5 {
    let t = Instant::now();
    let ctx = browser.new_context();
    let page = ctx.new_page().await.unwrap();
    page.goto("data:text/html,<title>T</title><button id='b'>Go</button>", None).await.unwrap();
    page.locator("#b").click().await.unwrap();
    let _ = page.locator("#b").text_content().await.unwrap();
    ctx.close().await.ok();
    eprintln!("  cycle {i}: {}ms", t.elapsed().as_millis());
  }

  eprintln!("=== DONE ===");
  browser.close().await.ok();
}
