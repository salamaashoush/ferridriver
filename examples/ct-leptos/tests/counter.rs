//! Counter component tests using the custom harness.

use ferridriver_ct_leptos::prelude::*;

#[component_test]
async fn counter_starts_at_zero(page: Page) -> Result<(), TestFailure> {
  expect(&page.locator("#count")).to_have_text("0").await?;
  Ok(())
}

#[component_test]
async fn counter_increments(page: Page) -> Result<(), TestFailure> {
  page.locator("#inc").click().await?;
  expect(&page.locator("#count")).to_have_text("1").await?;
  Ok(())
}

#[component_test]
async fn counter_decrements(page: Page) -> Result<(), TestFailure> {
  page.locator("#dec").click().await?;
  expect(&page.locator("#count")).to_have_text("-1").await?;
  Ok(())
}

#[component_test]
async fn counter_multiple_clicks(page: Page) -> Result<(), TestFailure> {
  for _ in 0..5 {
    page.locator("#inc").click().await?;
  }
  expect(&page.locator("#count")).to_have_text("5").await?;
  Ok(())
}

ferridriver_ct_leptos::main!();
