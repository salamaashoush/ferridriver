#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `[test].moduleAliases`: extra import specifiers served by the native
//! module loader. Both consumption paths — bundled (rolldown must mark
//! the alias EXTERNAL so the bytecode re-links by name) and dynamic
//! `import()` from a plain script (resolver + loader chain).
//!
//! One test fn: the alias map lives in a process-global slot and this
//! integration test binary is its own process.

use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session, bundle_and_compile,
  set_module_aliases,
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
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  }
}

#[tokio::test]
async fn aliased_specifiers_resolve_in_bundles_and_dynamic_imports() {
  set_module_aliases([
    ("@playwright/test".to_string(), "@ferridriver/test".to_string()),
    ("playwright".to_string(), "ferridriver".to_string()),
  ])
  .expect("install aliases");

  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("main.ts");
  std::fs::write(
    &entry,
    "import { test, expect } from '@playwright/test';\n\
     import { bdd, chromium } from 'playwright';\n\
     export default {\n\
       expectIsFn: typeof expect === 'function',\n\
       testDeclared: 'test' in { test },\n\
       bddIsObject: typeof bdd === 'object',\n\
       chromiumIsFn: typeof chromium === 'function',\n\
     };\n",
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
      serde_json::json!({
        "expectIsFn": true,
        "testDeclared": true,
        "bddIsObject": true,
        "chromiumIsFn": true,
      })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }

  // Same aliases through the sandbox loader chain (no bundler involved).
  let run2 = session
    .execute(
      r"
      const pw = await import('@playwright/test');
      const core = await import('playwright');
      return { expectIsFn: typeof pw.expect === 'function', host: core.host };
      ",
      &[],
      RunOptions::default(),
      &context,
    )
    .await;
  match run2.result.outcome {
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({ "expectIsFn": true, "host": "script" })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }

  // An unaliased bare specifier still fails to resolve — aliasing is
  // opt-in, not a catch-all that swallows genuinely missing packages.
  let missing = dir.path().join("missing.ts");
  std::fs::write(
    &missing,
    "import x from '@playwright/experimental-ct-react';\nexport default x;\n",
  )
  .expect("missing");
  assert!(
    bundle_and_compile(std::slice::from_ref(&missing), dir.path())
      .await
      .is_err(),
    "unaliased bare specifier must not resolve"
  );

  // Validation: the target must be a native module, and an alias must
  // never shadow one.
  let bad_target = set_module_aliases([("@playwright/test".to_string(), "playwright-core".to_string())])
    .expect_err("non-native target rejected");
  assert!(bad_target.contains("is not a native module"), "{bad_target}");
  let shadow =
    set_module_aliases([("ferridriver".to_string(), "@ferridriver/test".to_string())]).expect_err("shadow rejected");
  assert!(shadow.contains("already serves natively"), "{shadow}");

  // A rejected call leaves the previously installed map in place.
  let names: Vec<String> = ferridriver_script::module_aliases()
    .iter()
    .map(|(from, _)| from.clone())
    .collect();
  assert_eq!(names, vec!["@playwright/test".to_string(), "playwright".to_string()]);
}
