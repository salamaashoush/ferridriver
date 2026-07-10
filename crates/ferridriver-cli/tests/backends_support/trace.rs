//! Rule-9 integration tests for `context.tracing.start()` / `stop()`
//! (Playwright trace format VERSION 8) through QuickJS `run_script`, on
//! every backend. The assertions mirror the trace-viewer loader's hard
//! requirements (`packages/isomorphic/trace/traceLoader.ts` /
//! `traceModernizer.ts`): a `trace.trace` entry whose FIRST line is a
//! `context-options` event with `version: 8`, well-formed JSONL, action
//! events with callId/timing, and screencast frames whose `sha1` names
//! resolve to `resources/` entries.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// Record a trace with screenshots around a navigation + locator
/// actions, then validate the exported zip against the viewer's loader
/// rules.
pub fn test_tracing_records_viewer_loadable_zip(c: &mut McpClient) {
  let trace_path = std::env::temp_dir().join(format!("ferri-trace-{}-{}.zip", std::process::id(), c.backend));
  let _ = std::fs::remove_file(&trace_path);
  let v = c.script_value_with_args(
    r"
    const [tracePath] = args;
    await context.tracing.start({ title: 'rule9 trace', screenshots: true });
    await page.goto('data:text/html,<body><button id=b>Go</button></body>');
    await page.locator('#b').click();
    let missingError = '';
    try { await page.locator('#missing').click({ timeout: 500 }); } catch (e) { missingError = String(e); }
    await context.tracing.stop({ path: tracePath });
    let doubleStop = '';
    try { await context.tracing.stop(); } catch (e) { doubleStop = String(e); }
    return { missingError, doubleStop };
    ",
    serde_json::json!([trace_path.to_string_lossy()]),
  );
  assert!(
    !v["missingError"].as_str().unwrap_or("").is_empty(),
    "timed-out locator click must reject: {v}"
  );
  assert!(
    v["doubleStop"].as_str().unwrap_or("").contains("Must start tracing"),
    "stop without start must reject like Playwright: {v}"
  );

  let file = std::fs::File::open(&trace_path).expect("trace zip should be written");
  let mut archive = zip::ZipArchive::new(file).expect("valid zip");
  let names: Vec<String> = (0..archive.len())
    .map(|i| archive.by_index(i).expect("zip entry").name().to_string())
    .collect();
  assert!(
    names.iter().any(|n| n == "trace.trace"),
    "trace.trace required: {names:?}"
  );
  assert!(
    names.iter().any(|n| n == "trace.network"),
    "trace.network expected: {names:?}"
  );

  let mut trace = String::new();
  std::io::Read::read_to_string(&mut archive.by_name("trace.trace").expect("trace.trace"), &mut trace)
    .expect("read trace.trace");
  let lines: Vec<serde_json::Value> = trace
    .lines()
    .filter(|l| !l.trim().is_empty())
    .map(|l| serde_json::from_str(l).expect("every trace line must be valid JSON"))
    .collect();

  // Loader rule: the FIRST event must be context-options with version 8,
  // else everything is mis-modernized as v6.
  let first = &lines[0];
  assert_eq!(first["type"].as_str(), Some("context-options"), "first line: {first}");
  assert_eq!(first["version"].as_u64(), Some(8), "format version: {first}");
  assert_eq!(first["origin"].as_str(), Some("library"), "origin: {first}");

  let actions: Vec<&serde_json::Value> = lines.iter().filter(|e| e["type"] == "action").collect();
  let goto = actions
    .iter()
    .find(|a| a["method"] == "goto")
    .expect("page.goto must be traced");
  assert!(
    goto["callId"].as_str().unwrap_or("").starts_with("call@"),
    "callId: {goto}"
  );
  assert!(
    goto["startTime"].as_f64().unwrap() <= goto["endTime"].as_f64().unwrap(),
    "monotonic action timing: {goto}"
  );
  let click = actions
    .iter()
    .find(|a| a["method"] == "click" && a["params"]["selector"] == "#b")
    .expect("locator click must be traced with its selector");
  assert_eq!(click["class"].as_str(), Some("Locator"), "class: {click}");
  let failed = actions
    .iter()
    .find(|a| a["params"]["selector"] == "#missing")
    .expect("failed click must be traced too");
  assert!(
    failed["error"]["message"].as_str().is_some(),
    "failed action must carry its error: {failed}"
  );

  // Screencast frames must resolve to zip resources.
  let frames: Vec<&serde_json::Value> = lines.iter().filter(|e| e["type"] == "screencast-frame").collect();
  assert!(!frames.is_empty(), "screenshots: true must capture at least one frame");
  for frame in frames {
    let name = frame["sha1"].as_str().expect("frame resource name");
    assert!(
      names.iter().any(|n| n == &format!("resources/{name}")),
      "frame resource {name} must exist in the zip: {names:?}"
    );
  }

  // trace.network must be valid resource-snapshot JSONL.
  let mut network = String::new();
  std::io::Read::read_to_string(
    &mut archive.by_name("trace.network").expect("trace.network"),
    &mut network,
  )
  .expect("read trace.network");
  for line in network.lines().filter(|l| !l.trim().is_empty()) {
    let entry: serde_json::Value = serde_json::from_str(line).expect("network line JSON");
    assert_eq!(
      entry["type"].as_str(),
      Some("resource-snapshot"),
      "network line: {entry}"
    );
  }
  std::fs::remove_file(&trace_path).ok();
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::trace::test_tracing_records_viewer_loadable_zip",
    test_tracing_records_viewer_loadable_zip,
  );
}
