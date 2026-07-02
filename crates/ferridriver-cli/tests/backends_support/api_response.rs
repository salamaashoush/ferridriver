//! Rule-9 integration test for `apiResponse.serverAddr()` (Playwright 1.61)
//! through QuickJS `run_script`, on every backend.
//!
//! Asserts a protocol-visible effect (the resolved peer address) that only
//! holds when the value is captured end-to-end, not merely that the call
//! didn't throw.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// `apiResponse.serverAddr()` reports the resolved peer address. Fetch
/// the localhost server and assert the loopback ip + the server's port.
pub fn test_api_response_server_addr(c: &mut McpClient) {
  let port = super::spawn_html_server();
  let v = c.script_value_with_args(
    r"
    const [url, expectedPort] = args;
    const resp = await request.get(url);
    const addr = resp.serverAddr();
    return {
      status: resp.status(),
      hasAddr: addr != null,
      ip: addr ? addr.ipAddress : null,
      portMatches: addr ? addr.port === expectedPort : false,
    };
    ",
    serde_json::json!([format!("http://127.0.0.1:{port}/api"), port]),
  );
  assert_eq!(v["status"].as_i64(), Some(200), "{v}");
  assert_eq!(v["hasAddr"].as_bool(), Some(true), "serverAddr must be present: {v}");
  assert_eq!(v["ip"].as_str(), Some("127.0.0.1"), "loopback ip expected: {v}");
  assert_eq!(
    v["portMatches"].as_bool(),
    Some(true),
    "serverAddr.port must match server: {v}"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::api_response::test_api_response_server_addr",
    test_api_response_server_addr,
  );
}
