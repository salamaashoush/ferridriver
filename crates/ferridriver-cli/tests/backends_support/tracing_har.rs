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
    // Pre-seed a cookie so the navigation request carries a Cookie
    // header for HAR request.cookies to parse.
    await context.addCookies([{ name: 'reqcookie', value: 'reqvalue', domain: '127.0.0.1', path: '/' }]);
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

  // The document entry carries the enriched HAR fields. `harTracer.ts`
  // populates serverIPAddress/_serverPort from the peer address, cookies
  // from Set-Cookie, httpVersion from the protocol, and dns/connect/ssl
  // timing phases.
  let doc = entries
    .iter()
    .find(|e| e["request"]["url"].as_str() == Some(&format!("http://127.0.0.1:{port}/page")))
    .unwrap_or_else(|| panic!("document entry must be present: {har}"));
  // Backend-agnostic: response cookies (Set-Cookie) and httpVersion.
  let resp_cookies = doc["response"]["cookies"].as_array().expect("response.cookies array");
  assert!(
    resp_cookies
      .iter()
      .any(|c| c["name"].as_str() == Some("harcookie") && c["value"].as_str() == Some("harvalue")),
    "Set-Cookie must be parsed into response.cookies: {doc}"
  );
  assert!(
    doc["response"]["httpVersion"].as_str().is_some_and(|v| !v.is_empty()),
    "response.httpVersion must be set: {doc}"
  );
  // Peer address and the dns/connect/ssl timing phases come from the CDP
  // Network domain; Firefox/BiDi and WebKit's inspector protocol do not
  // surface them (Playwright's own HAR omits them there too).
  if c.backend.starts_with("cdp") {
    assert_eq!(
      doc["serverIPAddress"].as_str(),
      Some("127.0.0.1"),
      "CDP entry must carry serverIPAddress: {doc}"
    );
    assert_eq!(
      doc["_serverPort"].as_u64(),
      Some(u64::from(port)),
      "CDP entry must carry _serverPort: {doc}"
    );
    let timings = &doc["timings"];
    for phase in ["dns", "connect", "ssl", "send", "wait", "receive"] {
      assert!(
        timings.get(phase).is_some_and(serde_json::Value::is_number),
        "timings.{phase} must be a number: {doc}"
      );
    }
  }
  // The document's page entry carries the captured <title>.
  let pages = har["log"]["pages"].as_array().expect("log.pages array");
  assert!(
    pages.iter().any(|p| p["title"].as_str() == Some("HAR Fixture Title")),
    "log.pages must carry the captured document title: {pages:?}"
  );

  // The navigation carries the pre-seeded cookie as a Cookie request
  // header; HAR parses it into request.cookies. WebKit's inspector
  // `requestWillBeSent` omits the Cookie header (no request extra-info
  // event, unlike CDP) and offers no raw-header fallback, so its request
  // cookies are unavailable — the same limitation Playwright's WebKit
  // HAR carries.
  if c.backend != "webkit" {
    let request_cookie_seen = entries.iter().any(|e| {
      e["request"]["url"].as_str() == Some(&format!("http://127.0.0.1:{port}/page"))
        && e["request"]["cookies"]
          .as_array()
          .is_some_and(|cs| cs.iter().any(|c| c["name"].as_str() == Some("reqcookie")))
    });
    assert!(
      request_cookie_seen,
      "the Cookie request header must be parsed into request.cookies: {har}"
    );
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
