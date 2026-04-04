//! TodoMVC component tests — idiomatic Playwright-style API.
//!
//! Run: `cargo test -p ct-leptos-todomvc --test todomvc`
//!
//! Uses ferridriver's parallel test runner:
//! - ONE `trunk build` (cached)
//! - N browsers × N workers (auto-detected from CPU count)
//! - Fresh page per test
//! - Auto-retrying expect assertions

use ferridriver_ct_leptos::prelude::*;

// ── Helpers ──

async fn add_todo(page: &Page, text: &str) -> Result<(), TestFailure> {
  page.locator("#new-todo").fill(text).await?;
  page.locator("#new-todo").press("Enter").await?;
  Ok(())
}

// ── Adding todos ──

#[component_test]
async fn add_single_todo(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Buy milk").await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label")).to_have_text("Buy milk").await?;
  Ok(())
}

#[component_test]
async fn add_multiple_todos(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Buy milk").await?;
  add_todo(&page, "Walk the dog").await?;
  add_todo(&page, "Write tests").await?;
  expect(&page.locator(".todo-list li")).to_have_count(3).await?;
  expect(&page.locator(".todo-list li:nth-child(1) label")).to_have_text("Buy milk").await?;
  expect(&page.locator(".todo-list li:nth-child(2) label")).to_have_text("Walk the dog").await?;
  expect(&page.locator(".todo-list li:nth-child(3) label")).to_have_text("Write tests").await?;
  Ok(())
}

#[component_test]
async fn empty_input_does_not_add(page: Page) -> Result<(), TestFailure> {
  page.locator("#new-todo").press("Enter").await?;
  expect(&page.locator(".todo-list li")).to_have_count(0).await?;
  Ok(())
}

#[component_test]
async fn input_clears_after_add(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Test").await?;
  expect(&page.locator("#new-todo")).to_have_value("").await?;
  Ok(())
}

// ── Item count ──

#[component_test]
async fn shows_item_count(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "One").await?;
  expect(&page.locator("#todo-count")).to_have_text("1 item left").await?;
  add_todo(&page, "Two").await?;
  expect(&page.locator("#todo-count")).to_have_text("2 items left").await?;
  Ok(())
}

// ── Completing todos ──

#[component_test]
async fn toggle_todo_complete(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Buy milk").await?;
  page.locator(".todo-list li:nth-child(1) .toggle").click().await?;
  expect(&page.locator(".todo-list li.completed")).to_have_count(1).await?;
  expect(&page.locator("#todo-count")).to_have_text("0 items left").await?;
  Ok(())
}

#[component_test]
async fn toggle_todo_uncomplete(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Buy milk").await?;
  page.locator(".todo-list li:nth-child(1) .toggle").click().await?;
  page.locator(".todo-list li:nth-child(1) .toggle").click().await?;
  expect(&page.locator("#todo-count")).to_have_text("1 item left").await?;
  Ok(())
}

// ── Deleting todos ──

#[component_test]
async fn delete_todo(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Delete me").await?;
  add_todo(&page, "Keep me").await?;
  page.locator(".todo-list li:nth-child(1) .destroy").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label")).to_have_text("Keep me").await?;
  Ok(())
}

// ── Filtering ──

#[component_test]
async fn filter_active(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Active todo").await?;
  add_todo(&page, "Completed todo").await?;
  page.locator(".todo-list li:nth-child(2) .toggle").click().await?;
  page.locator("#filter-active").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label")).to_have_text("Active todo").await?;
  Ok(())
}

#[component_test]
async fn filter_completed(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Active todo").await?;
  add_todo(&page, "Completed todo").await?;
  page.locator(".todo-list li:nth-child(2) .toggle").click().await?;
  page.locator("#filter-completed").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label")).to_have_text("Completed todo").await?;
  Ok(())
}

#[component_test]
async fn filter_all_shows_everything(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "One").await?;
  add_todo(&page, "Two").await?;
  page.locator(".todo-list li:nth-child(1) .toggle").click().await?;
  page.locator("#filter-active").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  page.locator("#filter-all").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(2).await?;
  Ok(())
}

// ── Clear completed ──

#[component_test]
async fn clear_completed(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Keep").await?;
  add_todo(&page, "Remove").await?;
  add_todo(&page, "Also remove").await?;
  page.locator(".todo-list li:nth-child(2) .toggle").click().await?;
  page.locator(".todo-list li:nth-child(3) .toggle").click().await?;
  page.locator("#clear-completed").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label")).to_have_text("Keep").await?;
  Ok(())
}

// ── Toggle all ──

#[component_test]
async fn toggle_all_completes_all(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "One").await?;
  add_todo(&page, "Two").await?;
  add_todo(&page, "Three").await?;
  page.locator("#toggle-all").click().await?;
  expect(&page.locator("#todo-count")).to_have_text("0 items left").await?;
  Ok(())
}

#[component_test]
async fn toggle_all_uncompletes_when_all_done(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "One").await?;
  add_todo(&page, "Two").await?;
  page.locator("#toggle-all").click().await?;
  expect(&page.locator("#todo-count")).to_have_text("0 items left").await?;
  page.locator("#toggle-all").click().await?;
  expect(&page.locator("#todo-count")).to_have_text("2 items left").await?;
  Ok(())
}

// ── Editing ──

#[component_test]
async fn edit_todo_on_double_click(page: Page) -> Result<(), TestFailure> {
  add_todo(&page, "Original text").await?;
  page.locator(".todo-list li:nth-child(1) label").dblclick().await?;
  expect(&page.locator(".edit-input")).to_be_visible().await?;
  page.locator(".edit-input").fill("Updated text").await?;
  page.locator(".edit-input").press("Enter").await?;
  expect(&page.locator(".todo-list li:nth-child(1) label")).to_have_text("Updated text").await?;
  Ok(())
}

// ── Harness ──
ferridriver_ct_leptos::main!();
