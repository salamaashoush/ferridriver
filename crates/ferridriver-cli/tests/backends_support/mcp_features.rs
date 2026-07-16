#![allow(clippy::too_many_lines, clippy::unwrap_used, clippy::expect_used)]
//! End-to-end tests for the rmcp-2.x server features exercised over real
//! MCP-over-stdio: tool annotations + titles (`tools/list`), `artifact://`
//! resource links from `screenshot` (`ContentBlock::ResourceLink` +
//! `resources/read` + `resources/list`), and per-call progress notifications
//! (SEP-2575) from `navigate` and `run_bdd`.

use serde_json::json;

use super::client::{McpClient, ok};

/// `tools/list` advertises the annotations and titles wired onto every
/// built-in tool: read-only observers are flagged read-only, mutating tools
/// are flagged open-world, and tab management is flagged destructive.
pub fn test_tools_list_annotations(c: &mut McpClient) {
  let resp = c.list_tools();
  let tools = resp["result"]["tools"].as_array().expect("tools array");
  let find = |name: &str| {
    tools
      .iter()
      .find(|t| t["name"].as_str() == Some(name))
      .unwrap_or_else(|| panic!("tool {name} not advertised: {resp}"))
  };

  let snapshot = find("snapshot");
  assert_eq!(snapshot["title"].as_str(), Some("Accessibility Snapshot"), "{snapshot}");
  assert_eq!(
    snapshot["annotations"]["readOnlyHint"].as_bool(),
    Some(true),
    "snapshot is read-only: {snapshot}"
  );

  let navigate = find("navigate");
  assert_eq!(navigate["title"].as_str(), Some("Navigate"));
  assert_eq!(
    navigate["annotations"]["openWorldHint"].as_bool(),
    Some(true),
    "navigate is open-world: {navigate}"
  );
  assert_eq!(navigate["annotations"]["readOnlyHint"].as_bool(), Some(false));

  let page = find("page");
  assert_eq!(
    page["annotations"]["destructiveHint"].as_bool(),
    Some(true),
    "page (close tab) is destructive: {page}"
  );

  // Every advertised tool carries a human title.
  for t in tools {
    assert!(t["title"].as_str().is_some(), "tool {} missing title", t["name"]);
  }
}

/// `screenshot` returns the inline image PLUS a `resource_link` pointing at
/// an `artifact://` URI; that URI is fetchable via `resources/read` and shows
/// up in `resources/list`.
pub fn test_screenshot_resource_link(c: &mut McpClient) {
  c.nav("<h1>Shot</h1>");
  c.script("await page.waitForSelector('h1'); return true;");
  let resp = c.call_tool("screenshot", json!({}));
  ok(&resp, "screenshot");
  let content = resp["result"]["content"].as_array().expect("content array");

  let link = content
    .iter()
    .find(|b| b["type"].as_str() == Some("resource_link"))
    .unwrap_or_else(|| panic!("screenshot should include a resource_link: {resp}"));
  let uri = link["uri"].as_str().expect("resource_link uri");
  assert!(uri.starts_with("artifact://screenshots/"), "artifact uri: {uri}");
  assert_eq!(link["mimeType"].as_str(), Some("image/png"), "{link}");

  // The link resolves through resources/read as a base64 PNG blob.
  let read = c.read_resource(uri);
  ok(&read, "resources/read artifact");
  let blob = read["result"]["contents"][0]["blob"]
    .as_str()
    .unwrap_or_else(|| panic!("artifact read should return a blob: {read}"));
  assert!(
    blob.starts_with("iVBOR"),
    "artifact bytes are a PNG: {}",
    &blob[..8.min(blob.len())]
  );

  // And it is enumerated by resources/list.
  let listed = c.list_resources();
  let resources = listed["result"]["resources"].as_array().expect("resources array");
  assert!(
    resources.iter().any(|r| r["uri"].as_str() == Some(uri)),
    "artifact should be listed: {uri}"
  );
}

/// `navigate` emits progress notifications (navigating → loaded → done) keyed
/// on the caller's progress token, ending at `progress == total`.
pub fn test_navigate_progress(c: &mut McpClient) {
  let (resp, progress) = c.call_tool_with_progress("navigate", json!({"url": super::client::data_url("<h1>P</h1>")}));
  ok(&resp, "navigate with progress");
  assert!(
    progress.len() >= 2,
    "navigate should emit multiple progress beats, got {}: {progress:?}",
    progress.len()
  );
  let last = progress.last().unwrap();
  let p = last["params"]["progress"].as_f64().expect("progress value");
  let total = last["params"]["total"].as_f64().expect("total value");
  assert!((p - total).abs() < f64::EPSILON, "final progress reaches total: {last}");
}

/// `run_bdd` emits one progress beat per scenario, so a two-scenario feature
/// yields a start beat plus two increments ending at `progress == total`.
pub fn test_run_bdd_progress(c: &mut McpClient) {
  let gherkin = "Feature: Progress\n  \
    Scenario: one\n    \
    Given I navigate to \"data:text/html,<h1>One</h1>\"\n    \
    Then \"h1\" should contain text \"One\"\n  \
    Scenario: two\n    \
    Given I navigate to \"data:text/html,<h1>Two</h1>\"\n    \
    Then \"h1\" should contain text \"Two\"\n";
  let (resp, progress) = c.call_tool_with_progress("run_bdd", json!({ "gherkin": gherkin }));
  ok(&resp, "run_bdd with progress");
  assert!(
    progress.len() >= 2,
    "run_bdd should emit per-scenario progress, got {}: {progress:?}",
    progress.len()
  );
  let last = progress.last().unwrap();
  assert_eq!(last["params"]["total"].as_f64(), Some(2.0), "total scenarios: {last}");
  assert_eq!(
    last["params"]["progress"].as_f64(),
    Some(2.0),
    "final progress reaches 2 scenarios: {last}"
  );
}

pub fn register(set: &mut crate::TestSet<'_>) {
  set.run(
    "backends_support::mcp_features::test_tools_list_annotations",
    test_tools_list_annotations,
  );
  set.run(
    "backends_support::mcp_features::test_screenshot_resource_link",
    test_screenshot_resource_link,
  );
  set.run(
    "backends_support::mcp_features::test_navigate_progress",
    test_navigate_progress,
  );
  set.run(
    "backends_support::mcp_features::test_run_bdd_progress",
    test_run_bdd_progress,
  );
}
