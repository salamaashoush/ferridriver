#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The sandbox-safe `process` global: default-deny `env`, inert
//! identity, neutered `exit`, opt-in node-compat. No browser.

use std::sync::Arc;

use ferridriver_script::{Outcome, PathSandbox, RunContext, RunOptions, ScriptCaps, ScriptEngine, ScriptEngineConfig};

async fn run(src: &str, caps: ScriptCaps) -> Outcome {
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ferridriver_script::ExtensionHost::Script,
    caps,
  };
  ScriptEngine::new(ScriptEngineConfig::default())
    .run(src, &[], RunOptions::default(), ctx)
    .await
    .outcome
}

fn val(o: &Outcome) -> &serde_json::Value {
  match o {
    Outcome::Ok { success } => &success.value,
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn env_is_empty_by_default_and_inert_identity_is_present() {
  let o = run(
    "return { keys: Object.keys(process.env), os: typeof process.platform, \
       arch: typeof process.arch, ver: process.version, hasNode: 'node' in process.versions };",
    ScriptCaps::default(),
  )
  .await;
  let v = val(&o);
  assert_eq!(v["keys"], serde_json::json!([]), "env default-deny");
  assert_eq!(v["os"], serde_json::json!("string"));
  assert_eq!(v["arch"], serde_json::json!("string"));
  assert!(v["ver"].as_str().unwrap_or("").starts_with("ferridriver-"), "{v}");
  assert_eq!(v["hasNode"], serde_json::json!(false), "no node-compat by default");
}

#[tokio::test(flavor = "multi_thread")]
async fn env_exposes_only_the_allow_list_intersected_with_real_env() {
  // Use the ambient PATH (always present) rather than mutating the
  // environment (set_var is `unsafe` in edition 2024 and racy).
  let caps = ScriptCaps::resolve(
    &["PATH".to_string(), "FERRI_DEFINITELY_ABSENT_VAR_xyz".to_string()],
    false,
  );
  let o = run(
    "return { allowed: typeof process.env.PATH, \
       allowedLen: (process.env.PATH ?? '').length > 0, \
       undeclared: process.env.HOME ?? null, \
       missing: process.env.FERRI_DEFINITELY_ABSENT_VAR_xyz ?? null };",
    caps,
  )
  .await;
  let v = val(&o);
  assert_eq!(v["allowed"], serde_json::json!("string"), "declared+present exposed");
  assert_eq!(v["allowedLen"], serde_json::json!(true));
  assert_eq!(v["undeclared"], serde_json::json!(null), "undeclared env not exposed");
  assert_eq!(
    v["missing"],
    serde_json::json!(null),
    "declared-but-absent not invented"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_is_neutered() {
  let o = run(
    "try { process.exit(2); return 'no throw'; } catch (e) { return String(e); }",
    ScriptCaps::default(),
  )
  .await;
  assert!(
    val(&o)
      .as_str()
      .unwrap_or("")
      .contains("not allowed in the ferridriver sandbox"),
    "{:?}",
    val(&o)
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn node_compat_is_opt_in() {
  let caps = ScriptCaps {
    node_compat: true,
    ..ScriptCaps::default()
  };
  let o = run("return process.versions.node ?? null;", caps).await;
  assert!(
    val(&o).as_str().unwrap_or("").contains("ferridriver-compat"),
    "node-compat shim present + honest: {:?}",
    val(&o)
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn env_object_is_frozen() {
  let o = run(
    "try { process.env.X = 'y'; } catch {} return process.env.X ?? 'still-unset';",
    ScriptCaps::default(),
  )
  .await;
  assert_eq!(val(&o), &serde_json::json!("still-unset"), "env is frozen");
}
