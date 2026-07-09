#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The top-level `extensions` config merges into the BDD step bundle:
//! one file can define an MCP tool AND contribute `Given`/`When`/`Then`
//! steps, and the BDD runner sees the steps exactly like a step file's.
//! Also proves `ferridriver.host` gating: the `bdd` branch runs, the
//! `mcp` branch does not. Browser-free (bundle + registry only; no
//! scenario execution).

use std::path::PathBuf;

use ferridriver_bdd::js::{JsBddSession, bundle_steps_with};

fn scratch() -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("ferri_bdd_ext_merge_{nanos}"));
  std::fs::create_dir_all(dir.join("steps")).expect("mkdir steps");
  std::fs::create_dir_all(dir.join("ext")).expect("mkdir ext");
  dir
}

#[tokio::test(flavor = "multi_thread")]
async fn extension_files_merge_into_the_step_bundle() {
  let dir = scratch();
  std::fs::write(dir.join("steps/plain.js"), "Given('a plain step', function () {});").expect("write step file");
  std::fs::write(
    dir.join("ext/tool_and_step.js"),
    "defineTool({ name: 'bdd.tool', handler: async () => 'x' });\n\
     if (ferridriver.host === 'bdd') { Given('an extension step', function () {}); }\n\
     if (ferridriver.host === 'mcp') { Given('an mcp-only step', function () {}); }",
  )
  .expect("write extension file");

  let bundle = bundle_steps_with(&["steps/**/*.js".to_string()], &["./ext".to_string()], &dir)
    .await
    .expect("bundle steps + extensions");
  let session = JsBddSession::load(bundle, &dir, serde_json::Value::Null)
    .await
    .expect("load bundle into BDD session");

  let registry = session.registry();
  let patterns: Vec<&str> = registry.steps().iter().map(|s| s.expression.as_str()).collect();
  assert!(
    registry.find_match("a plain step").is_ok(),
    "step-file step must register; got: {patterns:?}"
  );
  assert!(
    registry.find_match("an extension step").is_ok(),
    "extension-contributed step must register exactly like a step file's; got: {patterns:?}"
  );
  assert!(
    registry.find_match("an mcp-only step").is_err(),
    "the mcp host branch must not run under ferridriver.host === 'bdd'"
  );

  let _ = std::fs::remove_dir_all(&dir);
}
