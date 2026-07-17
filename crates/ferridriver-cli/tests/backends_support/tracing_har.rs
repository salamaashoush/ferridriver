//! Rule-9 integration test for the zip-packed HAR roundtrip through
//! QuickJS `run_script`, on every backend. Lives here (not in
//! `tests/e2e/tracing.test.ts` with the plain-HAR coverage) because the
//! archive's payload entries are DEFLATE-compressed and the QuickJS
//! sandbox has no inflater to validate their contents.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

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
  assert!(
    v["served"].as_str().is_some_and(|s| !s.trim().is_empty()),
    "recorded body must be replayed from the zip: {v}"
  );
  assert_eq!(
    v["missThrew"].as_bool(),
    Some(true),
    "notFound: 'abort' must abort unrecorded requests: {v}"
  );
  std::fs::remove_file(&zip_path).ok();
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::tracing_har::test_tracing_har_zip_roundtrip",
    test_tracing_har_zip_roundtrip,
  );
}
