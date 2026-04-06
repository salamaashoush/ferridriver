#![allow(
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
)]
//! Standalone profiling binary -- run with samply or strace.
//! Does 20 test cycles: ctx+page+navigate+click+text+close.

use std::sync::Arc;
use std::time::Instant;
use ferridriver::Browser;
use ferridriver::options::LaunchOptions;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
  let iters = 20;

  // Launch 4 browsers in parallel.
  let mut browsers = Vec::new();
  let handles: Vec<_> = (0..4)
    .map(|_| tokio::spawn(Browser::launch(LaunchOptions::default())))
    .collect();
  for h in handles {
    browsers.push(Arc::new(h.await.unwrap().unwrap()));
  }

  let total_tests = iters * 4;
  let start = Instant::now();

  // Run tests in parallel across 4 workers.
  let mut worker_handles = Vec::new();
  for (wid, browser) in browsers.iter().enumerate() {
    let b = Arc::clone(browser);
    worker_handles.push(tokio::spawn(async move {
      for i in 0..iters {
        let ctx = b.new_context();
        let page = ctx.new_page().await.unwrap();
        let url = format!(
          "data:text/html,<title>T{}</title><button id='b' onclick=\"this.textContent='d'\">Go</button>",
          wid * iters + i
        );
        page.goto(&url, None).await.unwrap();
        page.locator("#b").click().await.unwrap();
        let _ = page.locator("#b").text_content().await.unwrap();
        ctx.close().await.ok();
      }
    }));
  }

  for h in worker_handles {
    h.await.unwrap();
  }

  let elapsed = start.elapsed();
  let per_test = elapsed.as_millis() as f64 / total_tests as f64;
  let tps = total_tests as f64 / elapsed.as_secs_f64();
  eprintln!(
    "{total_tests} tests, 4 workers: {}ms total, {per_test:.1}ms/test, {tps:.1} tests/sec",
    elapsed.as_millis()
  );

  for b in &browsers {
    b.close().await.ok();
  }
}
