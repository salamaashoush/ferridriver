//! A page whose target dies without going through `Page::close` must not stay
//! the context's active page.
//!
//! `Page::close` prunes the context's page list itself, so the explicit path was
//! always fine. A target that goes away on its own -- tab closed in the browser
//! UI, `window.close()`, target detached or crashed -- ran no such cleanup: the
//! dead page stayed active and every later command went to a CDP session that no
//! longer existed, surfacing as `Session with given id not found` on some
//! unrelated later call rather than as a lost page.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferridriver::chromium;
use ferridriver::options::LaunchOptions;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Poll until `f` holds or the deadline passes. The close arrives as a CDP
/// event, so it is observable only after a round trip.
async fn wait_until<F: Fn() -> bool>(f: F, timeout: Duration) -> bool {
  let deadline = Instant::now() + timeout;
  while Instant::now() < deadline {
    if f() {
      return true;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
  }
  f()
}

async fn launch() -> ferridriver::Browser {
  chromium()
    .launch(LaunchOptions {
      headless: Some(true),
      ..Default::default()
    })
    .await
    .expect("launch chromium")
}

/// Destroy the target browser-side. Deliberately not `page.close()`, which
/// prunes the context list on the way out and so would not exercise this path.
async fn self_close(page: &Arc<ferridriver::Page>) {
  let _ = page
    .evaluate(
      "window.close()",
      ferridriver::protocol::SerializedArgument::default(),
      None,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn self_closed_page_is_reported_closed_and_dropped_as_active() {
  let browser = launch().await;
  let context = browser.new_context().await.expect("new context");
  let page = context.new_page().await.expect("new page");
  page.set_content("<h1>probe</h1>").await.expect("set content");

  assert!(!page.is_closed(), "a live page must not report closed");

  self_close(&page).await;

  assert!(
    wait_until(|| page.is_closed(), Duration::from_secs(10)).await,
    "a target destroyed by the browser must mark its page closed"
  );

  // With the page closed the context must no longer offer it as active -- that
  // is what lets the next caller reopen instead of talking to a dead session.
  let state = browser.state().clone();
  let active_is_closed = {
    let guard = state.read().await;
    guard
      .context(context.name())
      .ok()
      .and_then(|ctx| ctx.active_page().map(ferridriver::backend::AnyPage::is_closed))
  };
  assert_ne!(
    active_is_closed,
    Some(true),
    "a closed page must not remain the context's active page"
  );
}

/// The recovery half: after the only page dies out-of-band, opening a page on
/// the same context must succeed and be usable.
#[tokio::test(flavor = "multi_thread")]
async fn context_reopens_a_page_after_its_target_dies() {
  let browser = launch().await;
  let context = browser.new_context().await.expect("new context");
  let page = context.new_page().await.expect("new page");
  page.set_content("<h1>probe</h1>").await.expect("set content");

  self_close(&page).await;
  assert!(
    wait_until(|| page.is_closed(), Duration::from_secs(10)).await,
    "target must be reported gone before testing recovery"
  );

  let replacement = context.new_page().await.expect("reopen a page after the target died");
  assert!(!replacement.is_closed(), "the replacement page must be live");
  let value = replacement
    .evaluate("1 + 1", ferridriver::protocol::SerializedArgument::default(), None)
    .await
    .expect("replacement page must serve commands");
  assert_eq!(value.to_json_like(), Some(serde_json::json!(2)));
}
