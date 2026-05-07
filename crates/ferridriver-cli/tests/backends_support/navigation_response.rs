//! §3.1 Rule-9 integration tests: `page.goto` / `reload` / `goBack` /
//! `goForward` return the main-document `Response` across every
//! backend that can observe it.
//!
//! Each test drives the QuickJS `run_script` binding through the MCP
//! client so the whole stack runs end-to-end: Rust core navigation →
//! per-backend network listener → `NavRequestSlot` → `Response`
//! surfaced back to JS as a real class with `status()` / `ok()` /
//! `url()`.
//!
//! Backend coverage:
//!   * `cdp-pipe` / `cdp-raw` — observe real responses via CDP
//!     `Network.requestWillBeSent` + `Network.responseReceived`;
//!     assert `response.ok() === true`, `status() === 200`, `url()`
//!     matches.
//!   * `bidi` — observes responses via `network.beforeRequestSent` +
//!     `network.responseStarted`; same assertions as CDP.
//!   * `webkit` — stock `WKWebView` exposes no public API for
//!     main-document response headers/status (the
//!     `decidePolicyForNavigationResponse:` callback doesn't round-trip
//!     status/headers through our IPC, and the JS-fetch interceptor
//!     only observes user-script fetches). Documented in the §1.4
//!     backend gap matrix. The test asserts the backend honestly
//!     returns `null` rather than fabricating a placeholder.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;

/// `page.goto` returns a Response carrying the real status / url.
pub fn test_goto_returns_response(c: &mut McpClient) {
  super::network::with_stub_server(|base| {
    let landed = format!("{base}/landed");
    let script = format!(
      r#"
      const resp = await page.goto("{landed}");
      if (resp == null) {{
        return {{ responded: false }};
      }}
      return {{
        responded: true,
        status: resp.status(),
        ok: resp.ok(),
        url: resp.url(),
      }};
      "#,
      landed = landed,
    );
    let v = c.script_value(&script);
    if c.backend == "webkit" {
      assert_eq!(
        v["responded"].as_bool(),
        Some(false),
        "webkit has no public API for main-doc response; goto should honestly return null: {v}",
      );
      return;
    }
    assert_eq!(v["responded"].as_bool(), Some(true), "goto should return Response: {v}");
    assert_eq!(v["status"].as_i64(), Some(200), "status: {v}");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok: {v}");
    assert!(v["url"].as_str().is_some_and(|u| u.ends_with("/landed")), "url: {v}");
  });
}

/// `page.goto` follows redirects and returns the Response of the final
/// landed document (not the 302). Redirect chain observability is a
/// documented gap on WebKit — the test returns early with a typed
/// assertion there rather than skipping silently.
pub fn test_goto_follows_redirects(c: &mut McpClient) {
  super::network::with_stub_server(|base| {
    let redirect = format!("{base}/redirect");
    let landed = format!("{base}/landed");
    let script = format!(
      r#"
      const resp = await page.goto("{redirect}");
      if (resp == null) {{
        return {{ responded: false }};
      }}
      return {{
        responded: true,
        status: resp.status(),
        url: resp.url(),
      }};
      "#,
      redirect = redirect,
    );
    let v = c.script_value(&script);
    if c.backend == "webkit" {
      assert_eq!(
        v["responded"].as_bool(),
        Some(false),
        "webkit cannot observe main-doc responses (§1.4 gap): {v}",
      );
      return;
    }
    assert_eq!(v["responded"].as_bool(), Some(true), "goto should return Response: {v}");
    assert_eq!(
      v["status"].as_i64(),
      Some(200),
      "final landed status should be 200: {v}"
    );
    assert!(
      v["url"]
        .as_str()
        .is_some_and(|u| u == landed.as_str() || u.ends_with("/landed")),
      "url should be the final landed document, not the 302: {v}",
    );
  });
}

/// `page.goto` with a URL that fails at the network layer surfaces a
/// typed error. Same contract as Playwright — the promise rejects,
/// not a Response-with-status-0.
pub fn test_goto_network_failure(c: &mut McpClient) {
  // webkit: the JS-interceptor + WKWebView navigation path reject
  // differently for blocked main-doc loads, and stock WKWebView's
  // error surface is tracked separately. Skip here rather than
  // reproducing the JS-fetch failure path, which is already covered
  // by `test_network_request_failure`.
  if c.backend == "webkit" {
    return;
  }
  let script = r#"
    try {
      await page.goto("http://127.0.0.1:65531/unreachable");
      return { threw: false };
    } catch (e) {
      return { threw: true, message: String(e && e.message || e) };
    }
  "#;
  let v = c.script_value(script);
  assert_eq!(
    v["threw"].as_bool(),
    Some(true),
    "goto to an unreachable URL should reject: {v}",
  );
  let msg = v["message"].as_str().unwrap_or("");
  assert!(
    msg.contains("ERR_CONNECTION")
      || msg.contains("NS_ERROR")
      || msg.contains("failed")
      || msg.contains("refused")
      || msg.contains("Navigation"),
    "error should name the network failure: {msg}",
  );
}

/// `page.reload` returns the main-document Response of the reloaded
/// page.
pub fn test_reload_returns_response(c: &mut McpClient) {
  super::network::with_stub_server(|base| {
    let landed = format!("{base}/landed");
    let script = format!(
      r#"
      await page.goto("{landed}");
      const resp = await page.reload();
      if (resp == null) {{
        return {{ responded: false }};
      }}
      return {{
        responded: true,
        status: resp.status(),
        ok: resp.ok(),
        url: resp.url(),
      }};
      "#,
      landed = landed,
    );
    let v = c.script_value(&script);
    if c.backend == "webkit" {
      assert_eq!(
        v["responded"].as_bool(),
        Some(false),
        "webkit reload should honestly return null (§1.4 gap): {v}",
      );
      return;
    }
    assert_eq!(
      v["responded"].as_bool(),
      Some(true),
      "reload should return Response: {v}",
    );
    assert_eq!(v["status"].as_i64(), Some(200), "status: {v}");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok: {v}");
    assert!(v["url"].as_str().is_some_and(|u| u.ends_with("/landed")), "url: {v}");
  });
}

/// `page.goBack` / `page.goForward` return the main-document Response
/// of the target history entry.
pub fn test_history_traversal_returns_response(c: &mut McpClient) {
  super::network::with_stub_server(|base| {
    let landed = format!("{base}/landed");
    let api_users = format!("{base}/api/users");
    let script = format!(
      r#"
      await page.goto("{landed}");
      await page.goto("{api_users}");
      const back = await page.goBack();
      const fwd = await page.goForward();
      if (back == null || fwd == null) {{
        return {{
          backResponded: back != null,
          fwdResponded: fwd != null,
        }};
      }}
      return {{
        backResponded: true,
        fwdResponded: true,
        backStatus: back.status(),
        backUrl: back.url(),
        fwdStatus: fwd.status(),
        fwdUrl: fwd.url(),
      }};
      "#,
      landed = landed,
      api_users = api_users,
    );
    let v = c.script_value(&script);
    if c.backend == "webkit" {
      assert_eq!(
        v["backResponded"].as_bool(),
        Some(false),
        "webkit goBack should honestly return null: {v}",
      );
      assert_eq!(
        v["fwdResponded"].as_bool(),
        Some(false),
        "webkit goForward should honestly return null: {v}",
      );
      return;
    }
    assert_eq!(
      v["backResponded"].as_bool(),
      Some(true),
      "goBack should return Response: {v}",
    );
    assert_eq!(
      v["fwdResponded"].as_bool(),
      Some(true),
      "goForward should return Response: {v}",
    );
    assert_eq!(v["backStatus"].as_i64(), Some(200), "back status: {v}");
    assert!(
      v["backUrl"].as_str().is_some_and(|u| u.ends_with("/landed")),
      "back url: {v}",
    );
    assert_eq!(v["fwdStatus"].as_i64(), Some(200), "fwd status: {v}");
    assert!(
      v["fwdUrl"].as_str().is_some_and(|u| u.ends_with("/api/users")),
      "fwd url: {v}",
    );
  });
}
