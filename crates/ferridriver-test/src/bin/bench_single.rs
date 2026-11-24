//! Measure futex calls per operation.

use ferridriver::chromium;
use ferridriver::options::LaunchOptions;
use std::io::Write as _;
use std::time::Instant;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
  ferridriver_test::logging::init(1);
  let browser = chromium().launch(LaunchOptions::default()).await?;
  eprintln!("=== BROWSER LAUNCHED, starting test cycles ===");

  // Marker so strace -e write can see it
  std::io::stderr().write_all(b"MARKER_START\n")?;

  for i in 0..5 {
    let t = Instant::now();
    let ctx = browser.new_context().await?;
    let page = ctx.new_page().await?;
    page
      .goto("data:text/html,<title>T</title><button id='b'>Go</button>")
      .await?;
    page.locator("#b").click().await?;
    page.locator("#b").text_content().await?;
    ctx.close().await?;
    eprintln!("  cycle {i}: {}ms", t.elapsed().as_millis());
  }

  eprintln!("=== DONE ===");
  browser.close().await?;
  Ok(())
}
