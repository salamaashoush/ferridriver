//! `browser.bind()` / `browser.unbind()` end-to-end through QuickJS on every
//! backend (Rule 9).
//!
//! The script binds the live browser over a loopback TCP endpoint and returns
//! it; the Rust side then connects a real [`ferridriver_session::SessionClient`]
//! and runs scripts against the bound browser, proving two things at once: the
//! binding serves the same page the script set up, and a browser bound from
//! inside a script is SCRIPTABLE — the bind path installs a real script host,
//! not an inert registry entry. The page-visible effect (the snapshot text, the
//! url) only appears if the bound server is actually driving this browser.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ferridriver_script::{Outcome, ScriptResult};
use ferridriver_session::{Command, RUN_VERB, ScriptRequest, SessionClient};

use super::client::McpClient;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .unwrap()
    .block_on(fut)
}

/// Run `code` on the bound session and return its result value as a string.
async fn run_on(session: &mut SessionClient, id: u64, code: &str) -> String {
  let args = serde_json::to_value(ScriptRequest::source(code)).unwrap();
  let reply = session.call(Command::new(id, RUN_VERB, args)).await.unwrap();
  assert!(reply.ok, "run failed: {:?}", reply.error);
  let result: ScriptResult = serde_json::from_str(&reply.text).expect("run result decodes");
  match result.outcome {
    Outcome::Ok { success } => match success.value {
      serde_json::Value::String(s) => s,
      other => other.to_string(),
    },
    Outcome::Error { error } => panic!("script failed on the bound session: {}", error.message),
  }
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

    // A snapshot reaches the exact page the script navigated.
    let snap = run_on(&mut session, 1, "return await page.snapshotForAI();").await;
    assert!(
      snap.contains("session-bound"),
      "snapshot did not reflect the bound page: {snap}"
    );

    // …and so does the live page url.
    let url = run_on(&mut session, 2, "return page.url();").await;
    assert!(url.starts_with("data:"), "unexpected url: {url}");

    // The session's own globals persist between runs on the bound browser.
    run_on(&mut session, 3, "globalThis.bound = 'kept'; return null;").await;
    let kept = run_on(&mut session, 4, "return globalThis.bound;").await;
    assert_eq!(kept, "kept", "bound session did not keep its VM state");
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
