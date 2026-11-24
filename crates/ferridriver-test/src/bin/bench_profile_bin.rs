//! Standalone profiling binary -- run with samply or strace.
//! Does 20 test cycles: ctx+page+navigate+click+text+close.

use ferridriver::chromium;
use ferridriver::options::LaunchOptions;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
  ferridriver_test::logging::init(1);
  let iters: u32 = 20;

  // Launch 4 browsers in parallel.
  let mut browsers = Vec::new();
  let handles: Vec<_> = (0..4)
    .map(|_| tokio::spawn(chromium().launch(LaunchOptions::default())))
    .collect();
  for h in handles {
    browsers.push(Arc::new(h.await??));
  }

  let total_tests = iters * 4;
  let start = Instant::now();

  // Run tests in parallel across 4 workers.
  let mut worker_handles = Vec::new();
  for (wid, browser) in (0u32..).zip(&browsers) {
    let b = Arc::clone(browser);
    worker_handles.push(tokio::spawn(async move {
      for i in 0..iters {
        let ctx = b.new_context().await?;
        let page = ctx.new_page().await?;
        let url = format!(
          "data:text/html,<title>T{}</title><button id='b' onclick=\"this.textContent='d'\">Go</button>",
          wid * iters + i
        );
        page.goto(&url).await?;
        page.locator("#b").click().await?;
        page.locator("#b").text_content().await?;
        ctx.close().await?;
      }
      anyhow::Ok(())
    }));
  }

  for h in worker_handles {
    h.await??;
  }

  let elapsed = start.elapsed();
  let per_test = elapsed.as_secs_f64() * 1000.0 / f64::from(total_tests);
  let tps = f64::from(total_tests) / elapsed.as_secs_f64();
  eprintln!(
    "{total_tests} tests, 4 workers: {}ms total, {per_test:.1}ms/test, {tps:.1} tests/sec",
    elapsed.as_millis()
  );

  for b in &browsers {
    b.close().await?;
  }
  Ok(())
}
