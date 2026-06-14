//! `browser.bind()` / `browser.unbind()` end-to-end through QuickJS on every
//! backend (Rule 9).
//!
//! The script binds the live browser over a loopback TCP endpoint and returns
//! it; the Rust side then connects a real [`ferridriver_session::SessionClient`]
//! and drives session verbs against the bound browser, proving the binding
//! serves the same page the script set up. The page-visible effect (the
//! snapshot text, the url) only appears if the bound server is actually
//! driving this browser — not just that `bind()` returned a string.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferridriver_session::{Command, SessionClient};
use serde_json::json;

use super::client::McpClient;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap()
    .block_on(fut)
}

pub fn test_bind_serves_live_browser(c: &mut McpClient) {
  c.nav("<h1 id=greet>session-bound</h1>");

  // Bind over a loopback TCP endpoint (port 0 → OS-assigned) and return it.
  let value = c.script_value(
    r"
    const { endpoint } = await browser.bind('rule9-bind', { host: '127.0.0.1', port: 0 });
    return endpoint;
    ",
  );
  let endpoint = value.as_str().expect("bind returns endpoint string").to_string();
  assert!(
    endpoint.starts_with("ws://127.0.0.1:"),
    "unexpected endpoint: {endpoint}"
  );

  block_on(async {
    let mut session = SessionClient::connect(&endpoint)
      .await
      .expect("connect to bound endpoint");

    // snapshot verb reaches the exact page the script navigated.
    let snap = session.call(Command::new(1, "snapshot", json!({}))).await.unwrap();
    assert!(snap.ok, "snapshot failed: {:?}", snap.error);
    assert!(
      snap.text.contains("session-bound"),
      "snapshot did not reflect the bound page: {}",
      snap.text
    );

    // url verb returns the live page url.
    let url = session.call(Command::new(2, "url", json!({}))).await.unwrap();
    assert!(url.ok, "url failed: {:?}", url.error);
    assert!(url.text.starts_with("data:"), "unexpected url: {}", url.text);
  });

  // Unbind tears the server down; a fresh connection now fails.
  c.script_value("await browser.unbind(); return true;");
  block_on(async {
    let connect = SessionClient::connect(&endpoint).await;
    assert!(connect.is_err(), "endpoint should be dead after unbind");
  });
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::session_bind::test_bind_serves_live_browser",
    test_bind_serves_live_browser,
  );
}
