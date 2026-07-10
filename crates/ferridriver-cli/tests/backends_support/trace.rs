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
    await context.tracing.start({ title: 'rule9 trace', screenshots: true, snapshots: true });
    await page.goto('data:text/html,<body><style>button{color:red}</style><button id=b>Go</button></body>');
    await page.evaluate(`console.log('trace-console-probe', 42)`);
    // The observed console log is pushed by the same listener task that
    // spools trace console lines (trace push first) — once the probe is
    // visible here, its trace line is guaranteed to be in the spool.
    let consoleSeen = false;
    for (let i = 0; i < 200 && !consoleSeen; i++) {
      const msgs = page.consoleMessages({ filter: 'all' });
      consoleSeen = msgs.some(m => m.text().includes('trace-console-probe'));
      if (!consoleSeen) await page.waitForTimeout(25);
    }
    const page2 = await context.newPage();
    await page2.close();
    await page.evaluate(`document.styleSheets[0].insertRule('body{margin:0}')`);
    // Click-heavy stretch: each click repaints; the around-action burst
    // window must let more than one screencast frame through.
    await page.evaluate(`document.getElementById('b').addEventListener('click', () => {
      document.body.style.background = 'rgb(' + ((Math.random() * 255) | 0) + ',120,120)';
    })`);
    for (let i = 0; i < 5; i++) {
      await page.locator('#b').click();
    }
    let missingError = '';
    try { await page.locator('#missing').click({ timeout: 500 }); } catch (e) { missingError = String(e); }
    await context.tracing.stop({ path: tracePath });
    let doubleStop = '';
    try { await context.tracing.stop(); } catch (e) { doubleStop = String(e); }
    return { missingError, doubleStop, consoleSeen };
    ",
    serde_json::json!([trace_path.to_string_lossy()]),
  );
  assert_eq!(
    v["consoleSeen"].as_bool(),
    Some(true),
    "console probe must reach the observed log: {v}"
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

  // DOM snapshots: the click action must carry before/after snapshot
  // names, each resolving to a frame-snapshot event. Snapshots are
  // incremental ([[n, m]] subtree references), so the button's literal
  // markup only has to appear SOMEWHERE in the page's snapshot chain,
  // and the CSSOM mutation (insertRule) must be re-serialized into the
  // captured stylesheet text.
  let snapshots: Vec<&serde_json::Value> = lines.iter().filter(|e| e["type"] == "frame-snapshot").collect();
  assert!(!snapshots.is_empty(), "snapshots: true must capture frame-snapshots");
  for kind in ["beforeSnapshot", "afterSnapshot"] {
    let name = click[kind]
      .as_str()
      .unwrap_or_else(|| panic!("click must carry {kind}: {click}"));
    let snapshot = snapshots
      .iter()
      .find(|f| f["snapshot"]["snapshotName"].as_str() == Some(name))
      .unwrap_or_else(|| panic!("{kind} {name} must resolve to a frame-snapshot"));
    assert_eq!(
      snapshot["snapshot"]["isMainFrame"].as_bool(),
      Some(true),
      "main-frame snapshot: {snapshot}"
    );
    assert!(
      snapshot["snapshot"]["html"].is_array(),
      "snapshot html must be a NodeSnapshot tree: {snapshot}"
    );
  }
  let all_html: String = snapshots.iter().map(|f| f["snapshot"]["html"].to_string()).collect();
  assert!(
    all_html.contains("BUTTON"),
    "the button must appear in the page's snapshot chain"
  );
  assert!(
    all_html.contains("margin"),
    "the CSSOM insertRule mutation must be captured in stylesheet text"
  );

  // Console messages must land in the trace (the viewer's Console tab)
  // with the pageId the actions carry, so the viewer attributes them.
  let console_probe = lines
    .iter()
    .find(|e| e["type"] == "console" && e["text"].as_str().unwrap_or("").contains("trace-console-probe"))
    .expect("console.log must produce a trace console line");
  assert_eq!(
    console_probe["messageType"].as_str(),
    Some("log"),
    "console messageType: {console_probe}"
  );
  // Args previews: `console.log('...', 42)` -> two `{preview, value}`
  // entries; the numeric arg's value survives as a JSON primitive.
  let args = console_probe["args"]
    .as_array()
    .unwrap_or_else(|| panic!("console args array: {console_probe}"));
  assert_eq!(args.len(), 2, "console.log carried two args: {console_probe}");
  assert!(
    args
      .iter()
      .any(|a| a["value"].as_i64() == Some(42) || a["preview"].as_str() == Some("42")),
    "the numeric arg must survive as a preview/value: {console_probe}"
  );
  let goto_page_id = goto["pageId"].as_str().expect("goto must carry a pageId");
  assert_eq!(
    console_probe["pageId"].as_str(),
    Some(goto_page_id),
    "console pageId must match the action's: {console_probe}"
  );
  assert!(
    console_probe["time"].as_f64().unwrap_or(-1.0) >= 0.0,
    "console time: {console_probe}"
  );

  // Page lifecycle events (`event` lines, tracing.ts onPageOpen /
  // onPageClose): the mid-trace page open is recorded synchronously,
  // its close via the page's lossless event listener.
  let events: Vec<&serde_json::Value> = lines.iter().filter(|e| e["type"] == "event").collect();
  let opened = events
    .iter()
    .find(|e| e["method"] == "page")
    .expect("mid-trace newPage must record a 'page' event");
  assert_eq!(opened["class"].as_str(), Some("BrowserContext"), "class: {opened}");
  let opened_page_id = opened["params"]["pageId"].as_str().expect("page event pageId");
  assert!(
    events
      .iter()
      .any(|e| e["method"] == "pageClosed" && e["params"]["pageId"] == opened_page_id),
    "closing the page must record a 'pageClosed' event for the same pageId: {events:?}"
  );

  // Screencast frames must resolve to zip resources; the click-heavy
  // stretch runs under the around-action burst (throttle lifted for
  // 500ms at every action boundary), so a single steady-state frame is
  // a regression.
  let frames: Vec<&serde_json::Value> = lines.iter().filter(|e| e["type"] == "screencast-frame").collect();
  assert!(
    frames.len() > 1,
    "click-heavy scenario must yield more than one screencast frame, got {}",
    frames.len()
  );
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

/// Child-frame snapshots must inline into their parent: the snapshot
/// streamer's `markIframe` (fed by protocol-level frame-owner
/// resolution on every backend) rewrites the parent's `<iframe>` to
/// `src="/snapshot/<frameId>"`, which the viewer resolves to the child
/// frame's own snapshot instead of rendering a placeholder.
pub fn test_tracing_iframe_snapshots_inline(c: &mut McpClient) {
  let trace_path = std::env::temp_dir().join(format!("ferri-trace-iframe-{}-{}.zip", std::process::id(), c.backend));
  let _ = std::fs::remove_file(&trace_path);
  c.script_value_with_args(
    r"
    const [tracePath] = args;
    await context.tracing.start({ title: 'iframe trace', snapshots: true });
    await page.goto('data:text/html,<h1>parent</h1>');
    await page.setContent(`<h1 id=p>parent</h1><iframe name=kid srcdoc='<button id=c>child</button>'></iframe>`);
    // frameLocator enter-frame guarantees the child frame is attached
    // and its document live before the traced click captures snapshots.
    await page.frameLocator('iframe').locator('#c').waitFor({ timeout: 10000 });
    await page.locator('#p').click();
    await context.tracing.stop({ path: tracePath });
    return {};
    ",
    serde_json::json!([trace_path.to_string_lossy()]),
  );

  let file = std::fs::File::open(&trace_path).expect("trace zip should be written");
  let mut archive = zip::ZipArchive::new(file).expect("valid zip");
  let mut trace = String::new();
  std::io::Read::read_to_string(&mut archive.by_name("trace.trace").expect("trace.trace"), &mut trace)
    .expect("read trace.trace");
  let lines: Vec<serde_json::Value> = trace
    .lines()
    .filter(|l| !l.trim().is_empty())
    .map(|l| serde_json::from_str(l).expect("every trace line must be valid JSON"))
    .collect();

  let snapshots: Vec<&serde_json::Value> = lines.iter().filter(|e| e["type"] == "frame-snapshot").collect();
  let child = snapshots
    .iter()
    .find(|f| f["snapshot"]["isMainFrame"].as_bool() == Some(false))
    .expect("the iframe must be captured as its own frame-snapshot");
  let child_frame_id = child["snapshot"]["frameId"].as_str().expect("child frameId");
  let child_html = child["snapshot"]["html"].to_string();
  assert!(
    child_html.contains("BUTTON") || child_html.contains("child"),
    "child snapshot must contain the iframe's content: {child_html}"
  );

  // markIframe took effect: some main-frame snapshot serializes the
  // <iframe> with the /snapshot/<frameId> annotation the viewer
  // resolves to the child's snapshot.
  let annotation = format!("/snapshot/{child_frame_id}");
  let main_html: String = snapshots
    .iter()
    .filter(|f| f["snapshot"]["isMainFrame"].as_bool() == Some(true))
    .map(|f| f["snapshot"]["html"].to_string())
    .collect();
  assert!(
    main_html.contains(&annotation),
    "parent snapshot must annotate the iframe with {annotation}; main-frame html: {main_html}"
  );
  std::fs::remove_file(&trace_path).ok();
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::trace::test_tracing_records_viewer_loadable_zip",
    test_tracing_records_viewer_loadable_zip,
  );
  set.run(
    "backends_support::trace::test_tracing_iframe_snapshots_inline",
    test_tracing_iframe_snapshots_inline,
  );
}
