#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Native Cucumber `this.attach` / `this.log`: a step queues attachments
//! into the extension registry; `drain_attachments` hands them back
//! (bytes + media type) for the BDD layer to forward into the test
//! result. No JS shim — `attach`/`log` are native Rust `Function`s on
//! the per-scenario World.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, HookArg, InMemoryVars, PathSandbox, RunContext, ScenarioWorld, ScriptEngineConfig, Session,
  bundle_and_compile, collect_registry, drain_attachments, eval_bundle, invoke_hook, invoke_step, set_scenario_world,
};

#[tokio::test(flavor = "multi_thread")]
async fn this_attach_and_log_reach_drain_attachments() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("steps.js"),
    "Given('the step', async function () { \
       this.attach('hello', 'text/plain'); \
       this.log('a note'); \
       this.attach({ k: 1 }); \
     });",
  )
  .expect("write steps");

  let bundle = bundle_and_compile(&[dir.path().join("steps.js")], dir.path())
    .await
    .expect("bundle");

  let sandbox = PathSandbox::new(dir.path()).expect("sandbox");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(sandbox),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ExtensionHost::Bdd,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session");
  let actx = session.async_context();
  eval_bundle(&actx, &bundle).await.expect("eval bundle");

  let reg = collect_registry(&actx).await.expect("collect");
  assert_eq!(reg.steps.len(), 1, "one step registered");

  set_scenario_world(&actx, &ScenarioWorld::default())
    .await
    .expect("world");
  invoke_step(&actx, 0, &[], None, None, &bundle.module_name)
    .await
    .expect("step ran");

  let mut atts = drain_attachments(&actx).await.expect("drain");
  assert_eq!(atts.len(), 3, "two attach + one log");

  // 1. string -> text/plain
  assert_eq!(atts[0].media_type, "text/plain");
  assert_eq!(atts[0].bytes, b"hello");
  // 2. log -> cucumber log media
  assert_eq!(atts[1].media_type, "text/x.cucumber.log+plain");
  assert_eq!(atts[1].bytes, b"a note");
  // 3. object -> application/json
  assert_eq!(atts[2].media_type, "application/json");
  assert_eq!(
    String::from_utf8(std::mem::take(&mut atts[2].bytes)).unwrap(),
    r#"{"k":1}"#
  );

  // Drained: a second drain is empty.
  assert!(
    drain_attachments(&actx).await.expect("drain2").is_empty(),
    "attachments drained once"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn after_hook_receives_cucumber_result_arg() {
  // Cucumber screenshot-on-failure idiom: After(fn) receives
  // `{ pickle: { name, tags }, result: { status, message } }`.
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("steps.js"),
    "After(function (s) { \
       if (s.result.status === 'FAILED') { \
         this.attach('failed:' + s.pickle.name + ':' + s.result.message, 'text/plain'); \
       } \
     });",
  )
  .expect("write steps");

  let bundle = bundle_and_compile(&[dir.path().join("steps.js")], dir.path())
    .await
    .expect("bundle");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ExtensionHost::Bdd,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session");
  let actx = session.async_context();
  eval_bundle(&actx, &bundle).await.expect("eval");
  let reg = collect_registry(&actx).await.expect("collect");
  assert_eq!(reg.hooks.len(), 1, "one After hook");

  set_scenario_world(&actx, &ScenarioWorld::default())
    .await
    .expect("world");
  let arg = HookArg {
    name: "My scenario".to_string(),
    tags: vec!["@x".to_string()],
    status: "FAILED".to_string(),
    message: Some("boom".to_string()),
  };
  invoke_hook(&actx, 0, Some(&arg), &bundle.module_name)
    .await
    .expect("hook ran");

  let atts = drain_attachments(&actx).await.expect("drain");
  assert_eq!(atts.len(), 1, "hook attached on FAILED");
  assert_eq!(atts[0].media_type, "text/plain");
  assert_eq!(atts[0].bytes, b"failed:My scenario:boom");
}

#[tokio::test(flavor = "multi_thread")]
async fn define_parameter_type_transformer_yields_typed_arg() {
  // defineParameterType transformer runs on the matched text at step
  // invocation and the step receives the typed value (cucumber parity).
  use ferridriver_script::{JsArg, invoke_step};
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("steps.js"),
    "defineParameterType({ name: 'amount', regexp: /\\d+/, \
       transformer: (s) => ({ n: Number(s) * 2 }) }); \
     Given('I have {amount}', async function (a) { this.attach(JSON.stringify(a), 'application/json'); });",
  )
  .expect("write steps");

  let bundle = bundle_and_compile(&[dir.path().join("steps.js")], dir.path())
    .await
    .expect("bundle");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ExtensionHost::Bdd,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session");
  let actx = session.async_context();
  eval_bundle(&actx, &bundle).await.expect("eval");
  let reg = collect_registry(&actx).await.expect("collect");
  assert_eq!(reg.steps.len(), 1);
  assert_eq!(reg.param_types.len(), 1, "param type registered");

  set_scenario_world(&actx, &ScenarioWorld::default())
    .await
    .expect("world");
  invoke_step(
    &actx,
    0,
    &[JsArg::Custom {
      type_name: "amount".to_string(),
      raw: "21".to_string(),
    }],
    None,
    None,
    &bundle.module_name,
  )
  .await
  .expect("step ran");

  let atts = drain_attachments(&actx).await.expect("drain");
  assert_eq!(atts.len(), 1);
  assert_eq!(
    String::from_utf8(atts[0].bytes.clone()).unwrap(),
    r#"{"n":42}"#,
    "transformer produced a typed object (21*2)"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn set_definition_function_wrapper_wraps_steps() {
  use ferridriver_script::invoke_step;
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("steps.js"),
    "setDefinitionFunctionWrapper(function (fn) { \
       return async function (...a) { this.attach('before', 'text/plain'); \
         const r = await fn.apply(this, a); this.attach('after', 'text/plain'); return r; }; }); \
     Given('s', async function () { this.attach('inner', 'text/plain'); });",
  )
  .expect("write");
  let bundle = bundle_and_compile(&[dir.path().join("steps.js")], dir.path())
    .await
    .expect("bundle");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ExtensionHost::Bdd,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session");
  let actx = session.async_context();
  eval_bundle(&actx, &bundle).await.expect("eval");
  set_scenario_world(&actx, &ScenarioWorld::default())
    .await
    .expect("world");
  invoke_step(&actx, 0, &[], None, None, &bundle.module_name)
    .await
    .expect("step");
  let atts = drain_attachments(&actx).await.expect("drain");
  let seq: Vec<String> = atts
    .iter()
    .map(|a| String::from_utf8(a.bytes.clone()).unwrap())
    .collect();
  assert_eq!(
    seq,
    vec!["before", "inner", "after"],
    "wrapper ran around the step body"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn per_step_timeout_option_is_enforced() {
  use ferridriver_script::{ScriptErrorKind, invoke_step};
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("steps.js"),
    "Given('slow', { timeout: 30 }, async function () { await new Promise(() => {}); });",
  )
  .expect("write");
  let bundle = bundle_and_compile(&[dir.path().join("steps.js")], dir.path())
    .await
    .expect("bundle");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    trusted_modules: false,
    host: ExtensionHost::Bdd,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session");
  let actx = session.async_context();
  eval_bundle(&actx, &bundle).await.expect("eval");
  set_scenario_world(&actx, &ScenarioWorld::default())
    .await
    .expect("world");
  let err = invoke_step(&actx, 0, &[], None, None, &bundle.module_name)
    .await
    .expect_err("step must time out");
  assert_eq!(err.kind, ScriptErrorKind::Timeout, "per-step {{timeout:30}} enforced");
}
