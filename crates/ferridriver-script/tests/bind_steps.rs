#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `bindSteps(ferridriver.test)` — the native primitive behind playwright-bdd's
//! `createBdd(test)`. A step registered through it records the fixture
//! chain it belongs to, so the BDD host can resolve the step's first
//! parameter from that chain.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, InMemoryVars, Outcome, RunContext, RunOptions, ScriptCaps, ScriptEngine, ScriptEngineConfig,
};

fn make_engine(host: ExtensionHost) -> (ScriptEngine, tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let context = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host,
    caps: ScriptCaps::default(),
    session: None,
  };
  (ScriptEngine::new(ScriptEngineConfig::default()), tmp, context)
}

async fn run_ok(src: &str) -> serde_json::Value {
  let (engine, _tmp, ctx) = make_engine(ExtensionHost::Bdd);
  match engine.run(src, &[], RunOptions::default(), ctx).await.outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("expected ok, got: {error:?}\nscript:\n{src}"),
  }
}

async fn run_err(src: &str) -> String {
  let (engine, _tmp, ctx) = make_engine(ExtensionHost::Bdd);
  match engine.run(src, &[], RunOptions::default(), ctx).await.outcome {
    Outcome::Ok { success } => panic!("expected error, got ok: {success:?}\nscript:\n{src}"),
    Outcome::Error { error } => format!("{error:?}"),
  }
}

#[tokio::test]
async fn bind_steps_returns_the_cucumber_surface() {
  let names = run_ok(
    "const bound = bindSteps(ferridriver.test);
     return Object.keys(bound).sort();",
  )
  .await;
  let names: Vec<String> = serde_json::from_value(names).expect("names");
  for expected in [
    "After",
    "AfterAll",
    "AfterStep",
    "And",
    "Before",
    "BeforeAll",
    "BeforeStep",
    "But",
    "Given",
    "Step",
    "Then",
    "When",
    "defineStep",
  ] {
    assert!(
      names.iter().any(|n| n == expected),
      "bindSteps did not expose {expected}: {names:?}"
    );
  }
}

#[tokio::test]
async fn a_bound_step_registers_and_runs() {
  run_ok(
    "const { Given } = bindSteps(ferridriver.test);
     Given('I have {int} cukes', function () {});
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn bind_steps_refuses_anything_but_a_test_object() {
  for arg in ["{}", "5", "undefined", "(() => {})"] {
    let err = run_err(&format!("bindSteps({arg}); return 'unreached'")).await;
    assert!(
      err.contains("accepts a \\\"test\\\" function") || err.contains("accepts a \"test\" function"),
      "expected the bindSteps guard for `{arg}`, got: {err}"
    );
  }
}

#[tokio::test]
async fn a_bound_step_belongs_to_the_chain_it_was_bound_to() {
  // Every chain gets its own fixture set, and a step registered through
  // one records that set rather than the base.
  run_ok(
    "const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
     const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
     const one = bindSteps(a);
     const two = bindSteps(b);
     one.Given('from a', function () {});
     two.Given('from b', function () {});
     Given('ambient', function () {});
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn merge_tests_binds_both_chains() {
  run_ok(
    "const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
     const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
     const { Given, Before } = bindSteps(ferridriver.mergeTests(a, b));
     Given('needs both', async function ({ alpha, beta }) {});
     Before(async function ({ alpha }) {});
     return 'ok'",
  )
  .await;
}
