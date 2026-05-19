#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The sandbox-safe `process` global: default-deny `env`, inert
//! identity, neutered `exit`. No browser.

use std::sync::Arc;

use ferridriver_script::{
  ConsoleLevel, Outcome, PathSandbox, RunContext, RunOptions, ScriptCaps, ScriptEngine, ScriptEngineConfig,
};

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
  assert_eq!(
    v["hasNode"],
    serde_json::json!(false),
    "process.versions.node never present"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn env_exposes_only_the_allow_list_intersected_with_real_env() {
  // Use the ambient PATH (always present) rather than mutating the
  // environment (set_var is `unsafe` in edition 2024 and racy).
  let caps = ScriptCaps::resolve(&["PATH".to_string(), "FERRI_DEFINITELY_ABSENT_VAR_xyz".to_string()]);
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
async fn env_object_is_frozen() {
  let o = run(
    "try { process.env.X = 'y'; } catch {} return process.env.X ?? 'still-unset';",
    ScriptCaps::default(),
  )
  .await;
  assert_eq!(val(&o), &serde_json::json!("still-unset"), "env is frozen");
}

async fn run_full(src: &str) -> ferridriver_script::ScriptResult {
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
    caps: ScriptCaps::default(),
  };
  ScriptEngine::new(ScriptEngineConfig::default())
    .run(src, &[], RunOptions::default(), ctx)
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn stdout_stderr_write_route_into_console_capture() {
  let r = run_full(
    "const a = process.stdout.write('hello\\n'); \
     const b = process.stderr.write('boom'); \
     return { a, b, tty: process.stdout.isTTY };",
  )
  .await;
  let v = match &r.outcome {
    Outcome::Ok { success } => &success.value,
    Outcome::Error { error } => panic!("expected ok: {error:?}"),
  };
  assert_eq!(v["a"], serde_json::json!(true), "write returns true");
  assert_eq!(v["b"], serde_json::json!(true));
  assert_eq!(v["tty"], serde_json::json!(false), "not a TTY");
  let logged: Vec<_> = r.console.iter().map(|e| (&e.level, e.message.as_str())).collect();
  assert!(
    logged
      .iter()
      .any(|(l, m)| matches!(l, ConsoleLevel::Log) && *m == "hello"),
    "stdout.write -> console Log, trailing newline trimmed: {logged:?}"
  );
  assert!(
    logged
      .iter()
      .any(|(l, m)| matches!(l, ConsoleLevel::Error) && *m == "boom"),
    "stderr.write -> console Error: {logged:?}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn hrtime_bigint_and_diff() {
  let o = run(
    "const t0 = process.hrtime(); \
     for (let i = 0; i < 50000; i++) {} \
     const d = process.hrtime(t0); \
     const b0 = process.hrtime.bigint(); const b1 = process.hrtime.bigint(); \
     return { tuple: Array.isArray(t0) && t0.length === 2, \
       diffOk: d[0] >= 0 && d[1] >= 0, \
       bigintType: typeof process.hrtime.bigint(), \
       monotonic: b1 >= b0 };",
    ScriptCaps::default(),
  )
  .await;
  let v = val(&o);
  assert_eq!(v["tuple"], serde_json::json!(true), "hrtime() -> [s, ns]");
  assert_eq!(v["diffOk"], serde_json::json!(true), "hrtime(prev) non-negative diff");
  assert_eq!(
    v["bigintType"],
    serde_json::json!("bigint"),
    "hrtime.bigint() -> BigInt"
  );
  assert_eq!(v["monotonic"], serde_json::json!(true), "bigint clock monotonic");
}

#[tokio::test(flavor = "multi_thread")]
async fn next_tick_runs_as_a_fifo_microtask() {
  // Documented behaviour: process.nextTick is a microtask (FIFO via
  // queueMicrotask), NOT Node's separate higher-priority queue. Order
  // therefore follows scheduling order.
  let o = run(
    "const order = []; \
     process.nextTick(() => order.push('nexttick')); \
     Promise.resolve().then(() => order.push('promise')); \
     await Promise.resolve(); await Promise.resolve(); \
     return order;",
    ScriptCaps::default(),
  )
  .await;
  assert_eq!(
    val(&o),
    &serde_json::json!(["nexttick", "promise"]),
    "nextTick scheduled first runs first (FIFO microtask)"
  );
}
