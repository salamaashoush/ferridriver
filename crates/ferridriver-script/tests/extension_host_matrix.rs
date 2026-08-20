#![allow(clippy::expect_used, clippy::unwrap_used)]
//! One extension file, four hosts, one answer.
//!
//! `ExtensionHost` decides what a file's registrations are CONSUMED for
//! — tools under MCP, steps under BDD, `test`/`describe` under the test
//! runner — but never whether the file loads, what it may register, or
//! whether its package's requirements are met. Each host having its own
//! loading path is exactly how that stopped being true.

use std::path::PathBuf;
use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, ExtensionSpec, InMemoryVars, Outcome, RequirementEnv, RunContext, RunOptions, ScriptCaps,
  ScriptEngineConfig, Session,
};

const HOSTS: [ExtensionHost; 4] = [
  ExtensionHost::Mcp,
  ExtensionHost::Bdd,
  ExtensionHost::Test,
  ExtensionHost::Script,
];

fn scratch(tag: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_nanos());
  let dir = std::env::temp_dir().join(format!("ferri_host_matrix_{tag}_{nanos}"));
  std::fs::create_dir_all(&dir).expect("mkdir");
  dir
}

async fn gate_and_load(specs: &[ExtensionSpec]) -> (Vec<String>, Vec<ferridriver_script::ExtensionBinding>) {
  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  let gated = ferridriver_script::gate(specs, &env, ExtensionHost::Script);
  let blocked = gated.blocked.clone();
  let bindings = ferridriver_script::load_bindings(specs, &env, &caps.extension_policy, ExtensionHost::Script).await;
  (blocked, bindings)
}

async fn snapshot_under(
  host: ExtensionHost,
  dir: &std::path::Path,
  bindings: Vec<ferridriver_script::ExtensionBinding>,
) -> serde_json::Value {
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: dir.into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: bindings,
    host,
    caps: ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  // Everything the file registered, read back through the surfaces each
  // host consumes — so a host that quietly dropped one shows up here.
  let code = "return {
      tools: Object.keys(tools).sort(),
      steps: ferridriver.bdd.__stepCount ?? null,
      host: ferridriver.host,
    };";
  let run = session.execute(code, &[], RunOptions::default(), &ctx).await;
  match run.result.outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("{host:?}: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_extension_registers_the_same_under_every_host() {
  let dir = scratch("same");
  std::fs::write(
    dir.join("plug.ts"),
    "defineTool({ name: 'alpha', handler: async () => 'a' });\n\
     defineTool({ name: 'beta.nested', handler: async () => 'b' });\n\
     Given('a step from an extension', function () {});\n",
  )
  .expect("write extension");
  let specs = vec![ExtensionSpec {
    spec: "./plug.ts".to_string(),
    base_dir: dir.clone(),
  }];

  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  let (_g, _c, failures) =
    ferridriver_script::extension_load::load(&specs, &env, &caps.extension_policy, ExtensionHost::Script).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let (blocked, bindings) = gate_and_load(&specs).await;
  assert!(blocked.is_empty(), "nothing to block");
  assert_eq!(bindings.len(), 1, "the file must compile");

  let mut seen: Vec<(ExtensionHost, serde_json::Value)> = Vec::new();
  for host in HOSTS {
    let snap = snapshot_under(host, &dir, bindings.clone()).await;
    // `beta` is the namespace object a dotted tool name projects.
    assert_eq!(
      snap["tools"],
      serde_json::json!(["alpha", "beta", "beta.nested"]),
      "{host:?} must expose both tools"
    );
    assert_eq!(snap["host"], serde_json::json!(host.as_str()));
    seen.push((host, snap["tools"].clone()));
  }
  let first = &seen[0].1;
  for (host, tools) in &seen[1..] {
    assert_eq!(tools, first, "{host:?} registered a different set");
  }

  let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unmet_requirement_blocks_identically_under_every_host() {
  // The gate is host-neutral by construction now — it runs before a
  // session exists. This pins that: the same package, the same verdict,
  // and nothing of it loaded anywhere.
  let dir = scratch("blocked");
  std::fs::create_dir_all(dir.join("pkg")).expect("mkdir pkg");
  std::fs::write(
    dir.join("pkg/package.json"),
    r#"{"name":"needs-binary","ferridriver":{"entries":["index.ts"],"requires":{"commands":["ferri-not-a-real-binary"]}}}"#,
  )
  .expect("write package.json");
  std::fs::write(
    dir.join("pkg/index.ts"),
    "defineTool({ name: 'gated', handler: async () => 'g' });\n",
  )
  .expect("write entry");
  let specs = vec![ExtensionSpec {
    spec: "./pkg".to_string(),
    base_dir: dir.clone(),
  }];

  let (blocked, bindings) = gate_and_load(&specs).await;
  assert_eq!(blocked.len(), 1, "the package must be blocked once");
  assert!(bindings.is_empty(), "a blocked package compiles nothing");

  for host in HOSTS {
    let snap = snapshot_under(host, &dir, bindings.clone()).await;
    assert_eq!(
      snap["tools"],
      serde_json::json!([]),
      "{host:?} must expose nothing from a blocked package"
    );
  }

  let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn extraction_reports_what_each_host_would_see() {
  // A file branches on `ferridriver.host`, so its contribution IS a
  // function of the host. Extraction used to evaluate under `mcp` alone
  // and report only the tools it found there: the BDD steps, the test
  // fixtures and the script-host contributions of the very same file
  // were reported as "declares nothing".
  let dir = scratch("snapshot");
  std::fs::write(
    dir.join("branching.ts"),
    "import { test } from '@ferridriver/test';\n\
     if (ferridriver.host === 'mcp') { defineTool({ name: 'only.mcp', handler: async () => 1 }); }\n\
     if (ferridriver.host === 'bdd') { Given('a step only bdd sees', function () {}); Before(function () {}); }\n\
     if (ferridriver.host === 'test') { test.extend({ onlyTest: ['x', { option: true }] }); }\n\
     if (ferridriver.host === 'script') { defineTool({ name: 'only.script', handler: async () => 2 }); }\n",
  )
  .expect("write extension");
  let specs = vec![ExtensionSpec {
    spec: "./branching.ts".to_string(),
    base_dir: dir.clone(),
  }];

  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  let (_gated, compiled, failures) =
    ferridriver_script::extension_load::load(&specs, &env, &caps.extension_policy, ExtensionHost::Script).await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let snapshot = &compiled[0].snapshot;

  let mcp = snapshot.for_host("mcp").expect("mcp host snapshot");
  assert_eq!(mcp.tools.len(), 1, "mcp registers one tool");
  assert!(mcp.steps.is_empty(), "mcp branch registers no steps");

  let bdd = snapshot.for_host("bdd").expect("bdd host snapshot");
  assert_eq!(bdd.steps, vec!["Given a step only bdd sees".to_string()]);
  assert_eq!(bdd.hooks, vec!["Before".to_string()]);
  assert!(bdd.tools.is_empty(), "bdd branch registers no tools");

  let test_host = snapshot.for_host("test").expect("test host snapshot");
  assert_eq!(test_host.fixtures, vec!["onlyTest".to_string()]);

  let script = snapshot.for_host("script").expect("script host snapshot");
  assert_eq!(script.tools.len(), 1, "script registers its own tool");

  // The MCP consumer still reads exactly its own host's manifests.
  let mcp_json = compiled[0].manifests_json();
  assert!(mcp_json.contains("only.mcp"), "{mcp_json}");
  assert!(!mcp_json.contains("only.script"), "{mcp_json}");

  let _ = std::fs::remove_dir_all(&dir);
}
