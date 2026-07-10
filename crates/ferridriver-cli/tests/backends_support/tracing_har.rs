//! Rule-9 integration test for `context.tracing.startHar()` / `stopHar()`
//! (Playwright 1.60) through QuickJS `run_script`, on every backend.
//!
//! Asserts the navigated URL + a 200 response actually land in the written
//! HAR's `log.entries`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// `context.tracing.startHar(path)` records the context's network into a
/// HAR file written by `stopHar()`; the navigated URL + a 200 response
/// land in `log.entries`.
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
    return { done: true };
    ",
    serde_json::json!([format!("http://127.0.0.1:{port}/page"), har_str]),
  );
  assert_eq!(v["done"].as_bool(), Some(true), "record phase failed: {v}");
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

/// `startHar` to a `.zip` path packs `har.har` plus `<sha1>.<ext>` body
/// entries (default `attach` policy), and `routeFromHAR` replays the
/// archive offline: a fresh navigation to the recorded URL is served
/// from the zip after the origin server is gone.
pub fn test_tracing_har_zip_roundtrip(c: &mut McpClient) {
  let port = super::spawn_html_server();
  let zip_path = std::env::temp_dir().join(format!("ferri-har-{}-{port}.har.zip", std::process::id()));
  let _ = std::fs::remove_file(&zip_path);
  let zip_str = zip_path.to_string_lossy().to_string();

  let v = c.script_value_with_args(
    r"
    const [url, zipPath] = args;
    await context.tracing.startHar(zipPath);
    await page.goto(url);
    await context.tracing.stopHar();
    return { done: true };
    ",
    serde_json::json!([format!("http://127.0.0.1:{port}/page"), zip_str]),
  );
  assert_eq!(v["done"].as_bool(), Some(true), "record phase failed: {v}");

  // Inspect the archive: har.har + attached bodies referenced via _file.
  let file = std::fs::File::open(&zip_path).expect("HAR zip should be written");
  let mut archive = zip::ZipArchive::new(file).expect("valid zip");
  let names: Vec<String> = (0..archive.len())
    .map(|i| archive.by_index(i).expect("zip entry").name().to_string())
    .collect();
  assert!(
    names.iter().any(|n| n == "har.har"),
    "zip must contain har.har: {names:?}"
  );
  let har: serde_json::Value = {
    let mut entry = archive.by_name("har.har").expect("har.har entry");
    serde_json::from_reader(&mut entry).expect("valid HAR JSON")
  };
  let entries = har["log"]["entries"].as_array().expect("log.entries array");
  assert!(!entries.is_empty(), "zip HAR must record entries: {har}");
  let file_refs: Vec<String> = entries
    .iter()
    .filter_map(|e| e["response"]["content"]["_file"].as_str().map(String::from))
    .collect();
  // Firefox discards response bytes for non-intercepted responses
  // (`network.getData` → "no such network data"); Playwright's own BiDi
  // backend has the same hole, so a BiDi-recorded archive legitimately
  // carries no attached bodies. Every other backend must attach them.
  let bodies_available = c.backend != "bidi";
  if bodies_available {
    assert!(
      !file_refs.is_empty(),
      "attach policy (zip default) must reference bodies via _file: {har}"
    );
    for name in &file_refs {
      assert!(
        names.iter().any(|n| n == name),
        "_file {name:?} must exist as a zip entry: {names:?}"
      );
    }
    let mime_ok = entries
      .iter()
      .any(|e| e["response"]["content"]["mimeType"].as_str() == Some("text/html"));
    assert!(mime_ok, "recorded mimeType must survive header-case differences: {har}");
  }

  // Replay offline: routeFromHAR(zip) must serve the recorded document
  // for the SAME url without touching the network (fresh URL fails).
  let v = c.script_value_with_args(
    r"
    const [url, zipPath] = args;
    await context.routeFromHAR(zipPath, { notFound: 'abort' });
    await page.goto(url);
    const served = await page.evaluate(() => document.body.textContent);
    let missThrew = false;
    try {
      await page.goto('http://ferri-har-miss.test/none', { timeout: 3000 });
    } catch { missThrew = true; }
    await context.unrouteAll();
    return { served: String(served), missThrew };
    ",
    serde_json::json!([format!("http://127.0.0.1:{port}/page"), zip_path.to_string_lossy()]),
  );
  if bodies_available {
    assert!(
      v["served"].as_str().is_some_and(|s| !s.trim().is_empty()),
      "recorded body must be replayed from the zip: {v}"
    );
  }
  assert_eq!(
    v["missThrew"].as_bool(),
    Some(true),
    "notFound: 'abort' must abort unrecorded requests: {v}"
  );
  std::fs::remove_file(&zip_path).ok();
}

/// `context.routeFromHAR(path, { update: true })` records instead of
/// replaying; the HAR is written when the context closes.
pub fn test_route_from_har_update_records_on_close(c: &mut McpClient) {
  // WebKit: browser.newContext is unsupported (single-context backend);
  // the default MCP context can't be closed mid-session.
  if c.backend == "webkit" {
    return;
  }
  let port = super::spawn_html_server();
  let har_path = std::env::temp_dir().join(format!("ferri-har-upd-{}-{port}.har", std::process::id()));
  let _ = std::fs::remove_file(&har_path);
  let v = c.script_value_with_args(
    r"
    const [url, harPath] = args;
    const ctx = await browser.newContext({});
    const p = await ctx.newPage();
    await ctx.routeFromHAR(harPath, { update: true, updateContent: 'embed' });
    await p.goto(url);
    await ctx.close();
    return { done: true };
    ",
    serde_json::json!([format!("http://127.0.0.1:{port}/page"), har_path.to_string_lossy()]),
  );
  assert_eq!(v["done"].as_bool(), Some(true), "update phase failed: {v}");
  let contents = std::fs::read_to_string(&har_path).expect("updated HAR should be written on context close");
  let har: serde_json::Value = serde_json::from_str(&contents).expect("HAR should be valid JSON");
  let entries = har["log"]["entries"].as_array().expect("log.entries array");
  assert!(
    entries.iter().any(|e| e["request"]["url"]
      .as_str()
      .is_some_and(|u| u.contains(&format!("127.0.0.1:{port}")))),
    "updated HAR must contain the navigated URL: {contents}"
  );
  std::fs::remove_file(&har_path).ok();
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::tracing_har::test_tracing_start_har",
    test_tracing_start_har,
  );
  set.run(
    "backends_support::tracing_har::test_tracing_har_zip_roundtrip",
    test_tracing_har_zip_roundtrip,
  );
  set.run(
    "backends_support::tracing_har::test_route_from_har_update_records_on_close",
    test_route_from_har_update_records_on_close,
  );
}
