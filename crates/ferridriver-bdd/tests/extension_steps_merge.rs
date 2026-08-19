#![allow(clippy::expect_used, clippy::unwrap_used)]
//! An extension contributes BDD steps to the runner exactly like a step
//! file does — but as compiled, gated bytecode installed into the step
//! VM, not as source appended to the step bundle. One file can define an
//! MCP tool AND `Given`/`When`/`Then` steps, and `ferridriver.host`
//! decides which of its branches runs.
//!
//! Also covers the two rules the split brought with it: a file reachable
//! from BOTH the step globs and an extension entry registers its steps
//! ONCE, and a package the requirements gate blocks contributes nothing.
//! Browser-free (bundle + registry only; no scenario execution).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferridriver_bdd::js::{BddSessionSetup, JsBddSession, bundle_steps_with};
use ferridriver_script::{ExtensionSpec, RequirementEnv, ScriptCaps};

fn scratch(tag: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("ferri_bdd_ext_{tag}_{nanos}"));
  std::fs::create_dir_all(dir.join("steps")).expect("mkdir steps");
  std::fs::create_dir_all(dir.join("ext")).expect("mkdir ext");
  dir
}

/// Load through the same path `ferridriver bdd` takes: bundle the step
/// files, gate + compile the extensions, install both into one session.
async fn session_for(dir: &Path, globs: &[String], specs: &[ExtensionSpec]) -> JsBddSession {
  let bundle = bundle_steps_with(globs, specs, dir).await.expect("bundle steps");
  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  let bindings = ferridriver_script::load_bindings(
    specs,
    &env,
    &caps.extension_policy,
    ferridriver_script::ExtensionHost::Bdd,
  )
  .await;
  JsBddSession::load(
    bundle,
    dir,
    &BddSessionSetup {
      extensions: Arc::new(bindings),
      ..Default::default()
    },
  )
  .await
  .expect("load step session")
}

#[tokio::test(flavor = "multi_thread")]
async fn an_extension_contributes_steps_to_the_bdd_registry() {
  let dir = scratch("merge");
  std::fs::write(dir.join("steps/plain.js"), "Given('a plain step', function () {});").expect("write step file");
  std::fs::write(
    dir.join("ext/tool_and_step.js"),
    "defineTool({ name: 'bdd.tool', handler: async () => 'x' });\n\
     if (ferridriver.host === 'bdd') { Given('an extension step', function () {}); }\n\
     if (ferridriver.host === 'mcp') { Given('an mcp-only step', function () {}); }",
  )
  .expect("write extension file");

  let specs = vec![ExtensionSpec {
    spec: "./ext".to_string(),
    base_dir: dir.clone(),
  }];
  let session = session_for(&dir, &["steps/**/*.js".to_string()], &specs).await;

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

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_is_both_a_step_glob_and_an_extension_entry_registers_once() {
  // The union bundle used to dedup the two sources against each other.
  // Now that extensions load as bytecode instead, the overlap has to be
  // dropped from the step bundle explicitly — otherwise the file
  // evaluates twice in one VM and every scenario using its steps fails
  // Ambiguous.
  let dir = scratch("overlap");
  std::fs::write(dir.join("steps/shared.js"), "Given('a shared step', function () {});").expect("write shared file");

  let specs = vec![ExtensionSpec {
    spec: "./steps/shared.js".to_string(),
    base_dir: dir.clone(),
  }];
  let session = session_for(&dir, &["steps/**/*.js".to_string()], &specs).await;

  let registry = session.registry();
  let shared = registry
    .steps()
    .iter()
    .filter(|s| s.expression == "a shared step")
    .count();
  assert_eq!(shared, 1, "the overlapping file must register its step once");
  assert!(
    registry.find_match("a shared step").is_ok(),
    "one registration means the match is unambiguous"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_the_gate_blocks_contributes_no_steps() {
  // `requires.commands` names a binary that is not on PATH, so the
  // package cannot work as declared. Before the BDD host shared the
  // loader it had no gate at all: the package's steps registered anyway
  // and failed at the first call.
  let dir = scratch("blocked");
  std::fs::write(dir.join("steps/plain.js"), "Given('a plain step', function () {});").expect("write step file");
  std::fs::create_dir_all(dir.join("pkg")).expect("mkdir pkg");
  std::fs::write(
    dir.join("pkg/package.json"),
    r#"{"name":"blocked-pkg","ferridriver":{"entries":["index.js"],"requires":{"commands":["ferri-not-a-real-binary"]}}}"#,
  )
  .expect("write package.json");
  std::fs::write(dir.join("pkg/index.js"), "Given('a blocked step', function () {});").expect("write entry");

  let specs = vec![ExtensionSpec {
    spec: "./pkg".to_string(),
    base_dir: dir.clone(),
  }];
  let session = session_for(&dir, &["steps/**/*.js".to_string()], &specs).await;

  let registry = session.registry();
  assert!(
    registry.find_match("a plain step").is_ok(),
    "the step files are unaffected by another package being blocked"
  );
  assert!(
    registry.find_match("a blocked step").is_err(),
    "a package whose requirements are unmet must not register anything"
  );

  let _ = std::fs::remove_dir_all(&dir);
}
