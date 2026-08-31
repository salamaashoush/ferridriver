//! Run the ten live-page Lighthouse audits against a page and print
//! what they found.
//!
//! `cargo run -p ferridriver-perf --example page-quality -- <url>`
//!
//! Beside the accessibility example for the same reason: this crate
//! answers what a page cost, and these answer what is wrong with it.
//! `scripts/perf-diff/compare-page-quality.py` checks the result against
//! Lighthouse's own verdicts for the same page.

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
  let report = page.check_page_quality(None).await?;
  browser.close().await?;
  println!("{}", serde_json::to_string_pretty(&report)?);
  Ok(())
}
