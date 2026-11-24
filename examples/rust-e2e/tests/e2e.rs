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
#[expect(
  clippy::unnecessary_wraps,
  reason = "`#[fixture]` takes the teardown-carrying form only as `Result<Fixture<T>>`"
)]
fn seeded_users(_ctx: TestContext) -> ferridriver_test::Result<Fixture<Vec<String>>> {
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

/// Filters chain like the getters, and `string | RegExp` is one type
/// for locators and assertions alike.
#[ferritest]
async fn filters_and_regexes(page: Arc<Page>) {
  page
    .goto("data:text/html,<ul><li class='done'>Ship it <button>x</button></li><li>Write tests</li></ul>")
    .await?;
  let items = page.get_by_role("listitem");
  let shipped = items.filter().has_text(regex::Regex::new(r"(?i)^ship")?);
  expect(&shipped).to_have_class(regex::Regex::new(r"\bdone\b")?).await?;
  expect(&items.filter().has(page.get_by_role("button")))
    .to_have_count(1)
    .await?;
  expect(&items.filter().has_not_text("Ship"))
    .to_have_text("Write tests")
    .await?;
  expect(&page.get_by_role("list"))
    .to_match_aria_snapshot("- listitem: Write tests")
    .await?;
}

/// `?` takes any error, `Context` explains a missing value, and polling
/// uses the configured expect timeout unless told otherwise.
#[ferritest]
async fn errors_polling_and_attachments(page: Arc<Page>, test_info: Arc<TestInfo>) {
  page
    .goto("data:text/html,<p id='n'>0</p><script>setTimeout(() => n.textContent = '3', 300)</script>")
    .await?;
  let read = || async {
    page
      .locator("#n")
      .text_content()
      .await
      .ok()
      .flatten()
      .unwrap_or_default()
  };
  expect_poll(read).not().to_equal("0".to_string()).await?;
  let count: u32 = read().await.parse()?;
  assert_eq!(count, 3);
  let first_even = [1u32, 5, 8]
    .into_iter()
    .find(|n| n % 2 == 0)
    .context("no even number")?;
  assert_eq!(first_even, 8);
  test_info.attach("count", "text/plain", count.to_string()).await;
}

/// Script and wait helpers take their options as setters too.
#[ferritest]
async fn script_tags_and_function_waits(page: Arc<Page>) {
  page.goto("data:text/html,<p>ready?</p>").await?;
  page
    .add_script_tag()
    .content("setTimeout(() => { window.ready = 'yes'; }, 200)")
    .await?;
  page
    .wait_for_function("() => window.ready === 'yes'")
    .polling(50u64)
    .await?;
  let flag: String = page.eval("() => window.ready").await?;
  assert_eq!(flag, "yes");
  page
    .wait_for_function_with("n => document.querySelectorAll('p').length === n", &1)
    .timeout(2_000u64)
    .await?;
}

/// Repetitions whose `shows_cart` has run, so `pays` can prove it came second.
static CARTS_SHOWN: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

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
  async fn shows_cart(page: Arc<Page>, test_info: Arc<TestInfo>) {
    expect(&page.get_by_role("heading")).to_have_text("Cart").await?;
    super::CARTS_SHOWN
      .lock()
      .map_err(TestFailure::msg)?
      .push(test_info.repeat_each_index);
  }

  #[ferritest]
  async fn pays(page: Arc<Page>, test_info: Arc<TestInfo>) {
    page.get_by_role("button").name("Pay").click().await?;
    let shown = super::CARTS_SHOWN.lock().map_err(TestFailure::msg)?;
    assert!(
      shown.contains(&test_info.repeat_each_index),
      "serial tests run in source order"
    );
  }
}

ferridriver_test::main!();
