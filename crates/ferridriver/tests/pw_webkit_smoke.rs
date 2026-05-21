//! End-to-end smoke test for the Playwright `WebKit` backend.
//!
//! Skipped unless a Playwright `WebKit` binary is reachable via
//! `FERRIDRIVER_PW_WEBKIT` env var or the standard Playwright cache.
//! Run `npx playwright install webkit` once on the dev box to
//! populate the cache.
//!
//! The test exercises the full happy path:
//!   1. Spawn the binary with `--inspector-pipe --headless --no-startup-window`.
//!   2. Complete `Playwright.enable`.
//!   3. Create an ephemeral context + page.
//!   4. Attach to the page (waits for `Target.targetCreated`, opens
//!      target session, runs `Page.enable` / `Runtime.enable` / ...).
//!   5. Navigate to a `data:` URL and verify the load completes.
//!   6. Evaluate a trivial JS expression and check the return value.
//!   7. Close the page + browser.

use ferridriver::backend::pw_webkit::{Browser, LaunchConfig, locate_binary, page, protocol::CreateContextParams};

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
  let browser = match Browser::launch(&config).await {
    Ok(b) => b,
    Err(e) => {
      // Most common reason on Arch / non-Ubuntu distros: the bundled
      // MiniBrowser links against libicu74 which is absent on systems
      // that have already moved to icu75+. The binary aborts before
      // the Playwright.enable handshake completes, surfacing as
      // "transport closed". Treat as skipped so dev boxes without the
      // right library set can still run the rest of the suite.
      eprintln!("skipping: launch failed ({e}); is libicu74 missing? install AUR `icu74` on Arch");
      return;
    },
  };
  let info = browser.info().await.expect("info");
  eprintln!("PW WebKit info: {info}");

  let context_id = browser
    .create_context(CreateContextParams::default())
    .await
    .expect("create_context");
  eprintln!("context: {context_id}");

  let page_handle = page::create_attached(&browser, Some(&context_id))
    .await
    .expect("create_attached");

  let loader_id = page_handle
    .navigate("data:text/html,<h1>hello</h1>", None)
    .await
    .expect("navigate");
  eprintln!("loaderId: {loader_id:?}");

  let value = page_handle.evaluate("1 + 1").await.expect("evaluate");
  assert_eq!(value, serde_json::json!(2), "evaluate returned: {value}");

  let title_html = page_handle
    .evaluate("document.documentElement.outerHTML")
    .await
    .expect("evaluate outerHTML");
  assert!(
    title_html.as_str().is_some_and(|s| s.contains("hello")),
    "outerHTML should include the navigated content: {title_html}"
  );

  page_handle.close().await.expect("close page");
  browser.close().await.expect("close browser");
}
