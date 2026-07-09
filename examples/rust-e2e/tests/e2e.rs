//! Rust E2E test example: the `#[ferritest]` harness end to end.
//!
//! Run with a browser installed (`ferridriver install chromium`):
//!
//! ```sh
//! cargo test -p rust-e2e-example --test e2e -- --headless
//! cargo test -p rust-e2e-example --test e2e -- --grep dashboard --workers 1
//! ```
//!
//! See docs/rust-testing.md for the full authoring guide.

use ferridriver_test::prelude::*;

/// A custom fixture: seeded data shared per test, with cleanup.
#[fixture(scope = "test")]
async fn seeded_users(_ctx: TestContext) -> ferridriver_test::Result<Fixture<Vec<String>>> {
  let users = vec!["alice".to_string(), "bob".to_string()];
  Ok(Fixture::new(users).on_teardown(|users| async move {
    // Real fixtures would release external resources here.
    drop(users);
  }))
}

/// Typed fixture parameters: `page` is a built-in, `seeded_users` the
/// custom fixture above (parameter name = fixture name).
#[ferritest]
async fn lists_seeded_users(page: Arc<Page>, seeded_users: Arc<Vec<String>>) {
  page
    .goto("data:text/html,<ul><li>alice</li><li>bob</li></ul><h1>Users</h1>")
    .await?;
  expect(&page.get_by_role("heading")).to_have_text("Users").await?;
  let count: usize = page.eval("() => document.querySelectorAll('li').length").await?;
  assert_eq!(count, seeded_users.len());
}

/// Action builders: options chain as setters, no bags at call sites.
#[ferritest(tag = "smoke", viewport = "800x600")]
async fn fills_a_form(page: Arc<Page>) {
  page
    .goto("data:text/html,<input id='name'><button id='go'>Go</button><p id='out'></p>")
    .await?;
  page.fill("#name", "ferris").timeout(2_000u64).await?;
  page
    .eval::<()>(
      "() => document.querySelector('#go').addEventListener('click', () => { \
         document.querySelector('#out').textContent = document.querySelector('#name').value; })",
    )
    .await?;
  page.locator("#go").click().await?;
  expect(&page.locator("#out")).to_have_text("ferris").await?;
}

/// Event combinator: the waiter arms before the action runs.
#[ferritest]
async fn captures_console_from_click(page: Arc<Page>) {
  page
    .goto("data:text/html,<button onclick=\"console.log('clicked')\">go</button>")
    .await?;
  let (message, ()) = page.expect_console(|| page.click("button")).await?;
  assert_eq!(message.text(), "clicked");
}

/// Parameterized rows with readable names.
#[ferritest_each(data = [("alice", 5), ("bob", 3)], names = ["alice length", "bob length"])]
async fn name_lengths(page: Arc<Page>, name: &str, len: usize) {
  page.goto(&format!("data:text/html,<p>{name}</p>")).await?;
  let text: String = page.locator("p").eval("el => el.textContent").await?;
  assert_eq!(text.len(), len);
}

/// Serial suite: source order, one worker, stop on first failure.
#[ferritest_suite(mode = "serial")]
mod checkout_flow {
  use ferridriver_test::prelude::*;

  #[before_each]
  async fn open_cart(page: Arc<Page>) {
    page
      .goto("data:text/html,<h1>Cart</h1><button id='pay'>Pay</button>")
      .await?;
  }

  #[ferritest]
  async fn shows_cart(page: Arc<Page>) {
    expect(&page.get_by_role("heading")).to_have_text("Cart").await?;
  }

  #[ferritest]
  async fn pays(page: Arc<Page>) {
    page.get_by_role("button").name("Pay").click().await?;
  }
}

ferridriver_test::main!();
