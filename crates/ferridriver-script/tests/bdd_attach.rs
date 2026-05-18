#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Native Cucumber `this.attach` / `this.log`: a step queues attachments
//! into the extension registry; `drain_attachments` hands them back
//! (bytes + media type) for the BDD layer to forward into the test
//! result. No JS shim — `attach`/`log` are native Rust `Function`s on
//! the per-scenario World.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, InMemoryVars, PathSandbox, RunContext, ScenarioWorld, ScriptEngineConfig, Session, bundle_and_compile,
  collect_registry, drain_attachments, eval_bundle, invoke_step, set_scenario_world,
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
