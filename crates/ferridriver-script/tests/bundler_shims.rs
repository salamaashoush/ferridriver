#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `[bundler]` shims: operator-declared import aliases and inline
//! virtual modules, applied by the rolldown plugin to every bundle.
//! One test fn — the shims live in a process-global slot, and this
//! integration test binary is its own process.

use std::sync::Arc;

use ferridriver_script::bundle::{BundlerShims, set_bundler_shims};
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
async fn alias_and_virtual_modules_resolve_in_bundles() {
  let dir = tempfile::tempdir().expect("tempdir");

  // A TypeScript shim served under a bare legacy specifier.
  std::fs::write(
    dir.path().join("wdio-shim.ts"),
    "export const keys = (k: string): string => `key:${k}`;\n",
  )
  .expect("shim");

  let mut config = ferridriver_config::BundlerConfig::default();
  config
    .alias
    .insert("@wdio/utils".to_string(), "wdio-shim.ts".to_string());
  config.virtual_modules.insert(
    "box:env".to_string(),
    "export const env = 'staging'; export default env;".to_string(),
  );
  set_bundler_shims(BundlerShims::from_config(&config, dir.path()));

  let entry = dir.path().join("main.ts");
  std::fs::write(
    &entry,
    "import { keys } from '@wdio/utils';\n\
     import { env } from 'box:env';\n\
     export default `${keys('Enter')}|${env}`;\n",
  )
  .expect("entry");

  let bundle = bundle_and_compile(std::slice::from_ref(&entry), dir.path())
    .await
    .expect("bundle");

  // The alias target is a real file, so the freshness input set must
  // include it (an edited shim invalidates cached bytecode).
  let inputs = bundle.source_files(dir.path());
  assert!(
    inputs.iter().any(|p| p.ends_with("wdio-shim.ts")),
    "alias shim tracked as transitive input: {inputs:?}"
  );

  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let run = session
    .execute_module(&bundle, &[], RunOptions::default(), &context)
    .await;
  match run.result.outcome {
    Outcome::Ok { success, .. } => assert_eq!(success.value, serde_json::json!("key:Enter|staging")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }

  // Built-in virtual modules must win over operator shims: an alias for
  // 'ferridriver' is ignored and the framework module still resolves.
  let mut hijack = ferridriver_config::BundlerConfig::default();
  hijack
    .alias
    .insert("ferridriver".to_string(), "wdio-shim.ts".to_string());
  set_bundler_shims(BundlerShims::from_config(&hijack, dir.path()));
  let entry2 = dir.path().join("framework.ts");
  std::fs::write(
    &entry2,
    "import { bdd } from 'ferridriver';\nexport default typeof bdd;\n",
  )
  .expect("entry2");
  let bundle2 = bundle_and_compile(std::slice::from_ref(&entry2), dir.path())
    .await
    .expect("bundle2");
  let run2 = session
    .execute_module(&bundle2, &[], RunOptions::default(), &context)
    .await;
  match run2.result.outcome {
    Outcome::Ok { success, .. } => assert_eq!(
      success.value,
      serde_json::json!("object"),
      "framework virtual module must not be hijackable by an alias"
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }

  // Different shims => different cache key (the fingerprint salts it).
  let fp_a = BundlerShims::from_config(&config, dir.path()).fingerprint();
  let fp_b = BundlerShims::from_config(&hijack, dir.path()).fingerprint();
  assert_ne!(fp_a, fp_b, "shim edits must change the bundle cache key");
}
