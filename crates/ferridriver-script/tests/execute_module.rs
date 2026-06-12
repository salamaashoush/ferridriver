#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `Session::execute_module` + `bundle_and_compile`: the TypeScript /
//! `import` module path behind `ferridriver run <file>` and MCP
//! `run_script {path}`. The run result is the module's `default` export,
//! and `CompiledBundle::source_files` reports the transitive input set the
//! sandbox jail validates.

use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session, bundle_and_compile,
};

fn ctx(dir: &std::path::Path) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    plugins: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  }
}

#[tokio::test]
async fn ts_module_with_import_returns_default_export() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("helper.ts"),
    "export const triple = (n: number): number => n * 3;",
  )
  .expect("helper");
  let entry = dir.path().join("main.ts");
  std::fs::write(
    &entry,
    "import { triple } from './helper';\nconst v: number = triple(14);\nexport default v;",
  )
  .expect("entry");

  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir.path())
    .await
    .expect("bundle");

  // The jail-validation input set must include the entry AND the
  // transitive helper import.
  let inputs = bundle.source_files(dir.path());
  assert!(
    inputs.iter().any(|p| p.ends_with("main.ts")),
    "entry tracked: {inputs:?}"
  );
  assert!(
    inputs.iter().any(|p| p.ends_with("helper.ts")),
    "import tracked: {inputs:?}"
  );

  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let run = session
    .execute_module(&bundle, &[], RunOptions::default(), &context)
    .await;
  match run.result.outcome {
    Outcome::Ok { success, .. } => assert_eq!(success.value, serde_json::json!(42)),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn module_without_default_export_yields_null() {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("m.ts");
  std::fs::write(&entry, "export const x: number = 1;\nconst _y = x + 1;").expect("entry");
  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir.path())
    .await
    .expect("bundle");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let run = session
    .execute_module(&bundle, &[], RunOptions::default(), &context)
    .await;
  match run.result.outcome {
    Outcome::Ok { success, .. } => assert!(
      success.value.is_null(),
      "no default export -> null: {:?}",
      success.value
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn bundled_module_can_import_ferridriver_and_cucumber_shims() {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("main.ts");
  std::fs::write(
    &entry,
    r#"
      import { tool, bdd } from "ferridriver";
      import { Given } from "@cucumber/cucumber";

      export default {
        tool: typeof tool,
        bdd: typeof bdd.Given,
        same: Given === bdd.Given,
      };
    "#,
  )
  .expect("entry");

  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir.path())
    .await
    .expect("bundle");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let run = session
    .execute_module(&bundle, &[], RunOptions::default(), &context)
    .await;
  match run.result.outcome {
    Outcome::Ok { success, .. } => assert_eq!(
      success.value,
      serde_json::json!({ "tool": "function", "bdd": "function", "same": true })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}
