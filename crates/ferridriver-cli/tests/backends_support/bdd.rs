//! `run_bdd` MCP tool: end-to-end through the real server binary.
//!
//! Proves the MCP server runs Gherkin on the SAME live session as the
//! other tools — inline text with built-in Rust steps, failure capture,
//! and user-supplied JS/TS step files (rolldown -> QuickJS bundle). Runs
//! once (the tool is session-driven, not per backend).

use super::client::McpClient;
use serde_json::{Value, json};

pub fn run(c: &mut McpClient) {
  test_inline_builtin_steps(c);
  test_failure_is_reported(c);
  test_js_step_file(c);
  test_shares_live_session(c);
  test_engine_is_reused(c);
}

/// Find the structured run_bdd payload (the content block that parses to a
/// JSON object carrying `status` + `scenarios`).
fn payload(resp: &Value) -> Value {
  let contents = resp["result"]["content"].as_array().expect("content array");
  for c in contents {
    if let Some(t) = c["text"].as_str() {
      if let Ok(v) = serde_json::from_str::<Value>(t) {
        if v.get("status").is_some() && v.get("scenarios").is_some() {
          return v;
        }
      }
    }
  }
  panic!("no run_bdd payload in {resp}");
}

/// Inline Gherkin against the built-in Rust step library.
fn test_inline_builtin_steps(c: &mut McpClient) {
  let gherkin = "Feature: Smoke\n  \
    Scenario: data URL renders\n    \
    Given I navigate to \"data:text/html,<h1>Hello BDD</h1>\"\n    \
    Then \"h1\" should contain text \"Hello BDD\"\n";
  let resp = c.call_tool("run_bdd", json!({ "gherkin": gherkin }));
  let p = payload(&resp);
  assert_eq!(p["status"], "passed", "expected pass, got: {resp}");
  assert_eq!(p["passed"], 1, "{resp}");
  assert_eq!(p["failed"], 0, "{resp}");
}

/// A wrong assertion must surface as a captured failure, not an MCP error.
fn test_failure_is_reported(c: &mut McpClient) {
  let gherkin = "Feature: Smoke\n  \
    Scenario: wrong text\n    \
    Given I navigate to \"data:text/html,<h1>Hello</h1>\"\n    \
    Then \"h1\" should contain text \"Goodbye\"\n";
  let resp = c.call_tool("run_bdd", json!({ "gherkin": gherkin }));
  let p = payload(&resp);
  assert_eq!(p["status"], "failed", "expected failure, got: {resp}");
  assert_eq!(p["failed"], 1, "{resp}");
}

/// The whole point: run_bdd runs on the SAME live session as run_script.
/// run_script navigates the page; run_bdd then asserts the content WITHOUT
/// navigating — it only passes if it sees the page run_script left behind.
fn test_shares_live_session(c: &mut McpClient) {
  c.script("await page.goto('data:text/html,<h1>SharedSession</h1>');");
  let gherkin = "Feature: Shared session\n  \
    Scenario: BDD sees the run_script page\n    \
    Then \"h1\" should contain text \"SharedSession\"\n";
  let resp = c.call_tool("run_bdd", json!({ "gherkin": gherkin }));
  let p = payload(&resp);
  assert_eq!(
    p["status"], "passed",
    "run_bdd must run on the same live page run_script navigated: {resp}"
  );
  assert_eq!(p["passed"], 1, "{resp}");
}

/// The step engine is loaded ONCE and reused across run_bdd calls (same
/// step-set). Proof: a module-level counter in the step file persists only
/// if the VM is reused. Two calls with the SAME steps glob — the second
/// asserts the counter reached 2, which is only possible if the module
/// (and its `counter`) survived from the first call.
fn test_engine_is_reused(c: &mut McpClient) {
  let dir = std::env::temp_dir().join(format!("ferridriver-mcp-bdd-reuse-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let step = dir.join("counter_steps.js");
  std::fs::write(
    &step,
    "let counter = 0;\n\
     When(\"I increment the counter\", async () => { counter += 1; });\n\
     Then(\"the counter should be {string}\", async (_world, n) => {\n  \
       if (String(counter) !== n) throw new Error(`counter ${counter} != ${n}`);\n\
     });\n",
  )
  .unwrap();
  let steps = vec![step.to_string_lossy().into_owned()];

  let call = |c: &mut McpClient, n: &str| -> Value {
    let gherkin = format!(
      "Feature: Reuse\n  Scenario: count\n    When I increment the counter\n    Then the counter should be \"{n}\"\n"
    );
    c.call_tool("run_bdd", json!({ "gherkin": gherkin, "steps": steps }))
  };

  let r1 = call(c, "1");
  assert_eq!(payload(&r1)["status"], "passed", "first run should see counter==1: {r1}");
  // If the engine were rebuilt per call, the module would re-init counter to 0
  // and this would fail (it'd be 1, not 2). Passing proves the VM was reused.
  let r2 = call(c, "2");
  let _ = std::fs::remove_dir_all(&dir);
  assert_eq!(
    payload(&r2)["status"],
    "passed",
    "second run should see counter==2 (engine reused, module state persisted): {r2}"
  );
}

/// User-supplied JS step file loaded via the rolldown -> QuickJS bundle,
/// the same path the CLI's `--steps` uses. Files are written to a temp dir
/// and passed as absolute globs (the test's cwd is the crate dir, not the
/// workspace root).
fn test_js_step_file(c: &mut McpClient) {
  let dir = std::env::temp_dir().join(format!("ferridriver-mcp-bdd-test-{}", std::process::id()));
  std::fs::create_dir_all(&dir).unwrap();
  let step = dir.join("steps.js");
  let feature = dir.join("custom.feature");
  std::fs::write(
    &step,
    "Given(\"I am on a temp blank page\", async (world) => {\n  await world.page.goto(\"about:blank\");\n});\n",
  )
  .unwrap();
  std::fs::write(
    &feature,
    "Feature: MCP JS steps\n  Scenario: custom js step\n    \
     Given I am on a temp blank page\n    Then the URL should contain \"about:blank\"\n",
  )
  .unwrap();

  let resp = c.call_tool(
    "run_bdd",
    json!({
      "features": [feature.to_string_lossy()],
      "steps": [step.to_string_lossy()],
    }),
  );
  let p = payload(&resp);
  let _ = std::fs::remove_dir_all(&dir);
  assert_eq!(p["status"], "passed", "expected JS-step pass, got: {resp}");
  assert_eq!(p["passed"], 1, "{resp}");
  assert_eq!(p["failed"], 0, "{resp}");
}
