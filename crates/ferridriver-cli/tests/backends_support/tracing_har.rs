//! Rule-9 integration test for `context.tracing.startHar()` / `stopHar()`
//! (Playwright 1.60) through QuickJS `run_script`, on every backend.
//!
//! Asserts the navigated URL + a 200 response actually land in the written
//! HAR's `log.entries`, and that `tracing.start()` (trace .zip) reports a
//! typed Unsupported.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// `context.tracing.startHar(path)` records the context's network into a
/// HAR file written by `stopHar()`; the navigated URL + a 200 response
/// land in `log.entries`. `tracing.start()` (trace .zip) is Unsupported.
pub fn test_tracing_start_har(c: &mut McpClient) {
  let port = super::spawn_html_server();
  let har_path = std::env::temp_dir().join(format!("ferri-har-{}-{port}.har", std::process::id()));
  let _ = std::fs::remove_file(&har_path);
  let har_str = har_path.to_string_lossy().to_string();
  let v = c.script_value_with_args(
    r"
    const [url, harPath] = args;
    await context.tracing.startHar(harPath);
    await page.goto(url);
    await page.goto(url + '?second');
    await context.tracing.stopHar();
    let startThrew = false;
    try { await context.tracing.start(); } catch (e) { startThrew = true; }
    return { startThrew };
    ",
    serde_json::json!([format!("http://127.0.0.1:{port}/page"), har_str]),
  );
  assert_eq!(
    v["startThrew"].as_bool(),
    Some(true),
    "tracing.start (trace .zip) should be Unsupported: {v}"
  );
  let contents = std::fs::read_to_string(&har_path).expect("HAR file should be written");
  let har: serde_json::Value = serde_json::from_str(&contents).expect("HAR should be valid JSON");
  let entries = har["log"]["entries"].as_array().expect("log.entries array");
  let urls: Vec<String> = entries
    .iter()
    .filter_map(|e| e["request"]["url"].as_str().map(String::from))
    .collect();
  assert!(
    urls.iter().any(|u| u.contains(&format!("127.0.0.1:{port}"))),
    "HAR must contain the navigated URL: {urls:?}"
  );
  assert!(
    entries.iter().any(|e| e["response"]["status"].as_i64() == Some(200)),
    "HAR must record a 200 response: {contents}"
  );
  std::fs::remove_file(&har_path).ok();
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::tracing_har::test_tracing_start_har",
    test_tracing_start_har,
  );
}
