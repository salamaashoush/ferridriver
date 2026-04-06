#![allow(clippy::single_match_else, clippy::manual_let_else, clippy::doc_markdown)]
//! Live test against a running Vite+React dev server.
//!
//! Prerequisites: cd examples/ct-react && bun install && bun run dev
//! Or set VITE_URL env var to point to the running server.
//!
//! Skip if no server running: this test is opt-in via VITE_URL env.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_react_counter_live() {
  let url = match std::env::var("VITE_URL") {
    Ok(u) => u,
    Err(_) => {
      eprintln!("VITE_URL not set, skipping live React CT test");
      return;
    }
  };

  let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
    .await
    .unwrap();
  let page = browser.new_page_with_url(&url).await.unwrap();

  // Wait for React to render.
  tokio::time::sleep(std::time::Duration::from_millis(500)).await;

  // Initial state.
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "0", "initial count");

  // Click + three times.
  for _ in 0..3 {
    page.locator("#inc").click().await.unwrap();
  }
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "3", "after 3 increments");

  // Click - once.
  page.locator("#dec").click().await.unwrap();
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "2", "after decrement");

  let _ = browser.close().await;
}
