//! Audit a page for accessibility problems and print what axe-core found.
//!
//! `cargo run -p ferridriver-perf --example accessibility -- <url>`
//!
//! Lives beside the trace examples because it is the same job from the
//! other end: this crate answers what a page cost, and this answers who
//! it locked out. `scripts/perf-diff/compare-accessibility.py` checks
//! the result against Lighthouse's own verdicts for the same page.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let url = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "http://127.0.0.1:8732/rich/".into());
  let browser = ferridriver::browser_type::chromium()
    .launch(ferridriver::options::LaunchOptions {
      headless: Some(true),
      ..Default::default()
    })
    .await?;
  let context = browser.new_context().await?;
  let page = context.new_page().await?;
  page.goto(&url).await?;
  page.wait_for_load_state(None).await?;
  let report = page.check_accessibility(None).await?;
  browser.close().await?;
  println!("{}", serde_json::to_string_pretty(&report)?);
  Ok(())
}
