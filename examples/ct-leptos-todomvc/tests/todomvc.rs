//! TodoMVC E2E tests for Leptos -- uses ferridriver's #[ferritest] macro.
//!
//! Prerequisites: `trunk build` in this directory (produces dist/).
//! Run: `cargo test -p ct-leptos-todomvc --test todomvc`

use ferridriver::Page;
use ferridriver_test::expect::expect;
use ferridriver_test_macros::ferritest;

// ── Helpers ──

const APP_URL: &str = "http://127.0.0.1:8787";

async fn add_todo(page: &Page, text: &str) -> Result<(), String> {
  page.locator("#new-todo").fill(text).await?;
  page.locator("#new-todo").press("Enter").await?;
  Ok(())
}

// ── Adding todos ──

#[ferritest]
async fn add_single_todo(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Buy milk")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label"))
    .to_have_text("Buy milk")
    .await?;
}

#[ferritest]
async fn add_multiple_todos(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Buy milk")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Walk the dog")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Write tests")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  expect(&page.locator(".todo-list li")).to_have_count(3).await?;
}

#[ferritest]
async fn empty_input_does_not_add(page: Page) {
  page.goto(APP_URL, None).await?;
  page.locator("#new-todo").press("Enter").await?;
  expect(&page.locator(".todo-list li")).to_have_count(0).await?;
}

#[ferritest]
async fn input_clears_after_add(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Test")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  expect(&page.locator("#new-todo")).to_have_value("").await?;
}

// ── Item count ──

#[ferritest]
async fn shows_item_count(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "One")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  expect(&page.locator("#todo-count")).to_have_text("1 item left").await?;
  add_todo(&page, "Two")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  expect(&page.locator("#todo-count"))
    .to_have_text("2 items left")
    .await?;
}

// ── Completing todos ──

#[ferritest]
async fn toggle_todo_complete(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Buy milk")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  page.locator(".todo-list li:nth-child(1) .toggle").click().await?;
  expect(&page.locator(".todo-list li.completed"))
    .to_have_count(1)
    .await?;
  expect(&page.locator("#todo-count"))
    .to_have_text("0 items left")
    .await?;
}

// ── Deleting todos ──

#[ferritest]
async fn delete_todo(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Delete me")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Keep me")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  page.locator(".todo-list li:nth-child(1) .destroy").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label"))
    .to_have_text("Keep me")
    .await?;
}

// ── Filtering ──

#[ferritest]
async fn filter_active(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Active todo")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Completed todo")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  page.locator(".todo-list li:nth-child(2) .toggle").click().await?;
  page.locator("#filter-active").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label"))
    .to_have_text("Active todo")
    .await?;
}

#[ferritest]
async fn filter_completed(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Active todo")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Completed todo")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  page.locator(".todo-list li:nth-child(2) .toggle").click().await?;
  page.locator("#filter-completed").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label"))
    .to_have_text("Completed todo")
    .await?;
}

// ── Clear completed ──

#[ferritest]
async fn clear_completed(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Keep")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Remove")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  page.locator(".todo-list li:nth-child(2) .toggle").click().await?;
  page.locator("#clear-completed").click().await?;
  expect(&page.locator(".todo-list li")).to_have_count(1).await?;
  expect(&page.locator(".todo-list li label"))
    .to_have_text("Keep")
    .await?;
}

// ── Toggle all ──

#[ferritest]
async fn toggle_all_completes_all(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "One")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Two")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  add_todo(&page, "Three")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  page.locator("#toggle-all").click().await?;
  expect(&page.locator("#todo-count"))
    .to_have_text("0 items left")
    .await?;
}

// ── Editing ──

#[ferritest]
async fn edit_todo_on_double_click(page: Page) {
  page.goto(APP_URL, None).await?;
  add_todo(&page, "Original text")
    .await
    .map_err(|e| ferridriver_test::model::TestFailure::from(e))?;
  page.locator(".todo-list li:nth-child(1) label").dblclick().await?;
  expect(&page.locator(".edit-input")).to_be_visible().await?;
  page.locator(".edit-input").fill("Updated text").await?;
  page.locator(".edit-input").press("Enter").await?;
  expect(&page.locator(".todo-list li:nth-child(1) label"))
    .to_have_text("Updated text")
    .await?;
}
