//! Component test for the Leptos Counter.
//!
//! Requires `trunk` installed: `cargo install trunk`
//!
//! Run: `cargo test -p ct-leptos-example --test counter`

use ferridriver_ct_leptos::LeptosComponentTest;

#[tokio::test]
async fn test_counter_increments() {
  let ct = LeptosComponentTest::new(".")
    .csr()
    .start()
    .await
    .expect("failed to start Leptos dev server (is trunk installed?)");

  let page = ct.new_page().await.unwrap();

  // Initial state.
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "0", "initial count should be 0");

  // Click + three times.
  for _ in 0..3 {
    page.locator("#inc").click().await.unwrap();
  }
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "3", "count should be 3");

  // Click - once.
  page.locator("#dec").click().await.unwrap();
  let count = page.locator("#count").text_content().await.unwrap().unwrap_or_default();
  assert_eq!(count, "2", "count should be 2");

  ct.stop().await;
}
