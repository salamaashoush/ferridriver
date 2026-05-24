//! End-to-end smoke test for the Playwright `WebKit` backend.
//!
//! Skipped unless a Playwright `WebKit` binary is reachable via
//! `FERRIDRIVER_PW_WEBKIT` or the standard Playwright cache. Exercises
//! launch -> context -> page -> navigate -> evaluate -> close through
//! the high-level `PwWebKitBrowser` / `AnyPage` surface.

use ferridriver::backend::NavLifecycle;
use ferridriver::backend::pw_webkit::{LaunchConfig, PwWebKitBrowser, locate_binary};

fn binary_available() -> bool {
  locate_binary().is_ok()
}

#[tokio::test]
async fn pw_webkit_launch_navigate_evaluate() {
  let _ = tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "ferridriver=info".into()),
    )
    .with_writer(std::io::stderr)
    .try_init();

  if !binary_available() {
    eprintln!("skipping: no Playwright WebKit binary found (set FERRIDRIVER_PW_WEBKIT)");
    return;
  }

  let config = LaunchConfig {
    headless: true,
    ..LaunchConfig::default()
  };
  let mut browser = match PwWebKitBrowser::launch(&config).await {
    Ok(b) => b,
    Err(e) => {
      eprintln!("skipping: launch failed ({e}); is libicu74 missing? install AUR `icu74` on Arch");
      return;
    },
  };
  eprintln!("PW WebKit version: {}", browser.version());

  let context_id = browser.new_context(None).await.expect("new_context");
  let page = browser
    .new_page("data:text/html,<h1>hello</h1>", Some(&context_id), None)
    .await
    .expect("new_page");

  let _ = page
    .goto("data:text/html,<h1>hello</h1>", NavLifecycle::Load, 30_000, None)
    .await;

  let value = page.evaluate("1 + 1").await.expect("evaluate");
  assert_eq!(value, Some(serde_json::json!(2)), "evaluate returned: {value:?}");

  let html = page
    .evaluate("document.documentElement.outerHTML")
    .await
    .expect("evaluate outerHTML");
  assert!(
    html
      .as_ref()
      .and_then(|v| v.as_str())
      .is_some_and(|s| s.contains("hello")),
    "outerHTML should include the navigated content: {html:?}"
  );

  page
    .close_page(ferridriver::options::PageCloseOptions::default())
    .await
    .expect("close page");
  browser.close().await.expect("close browser");
}
