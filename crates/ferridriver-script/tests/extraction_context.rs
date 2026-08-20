#![allow(clippy::expect_used, clippy::unwrap_used)]
//! What the manifest-extraction pass must agree with the session about.
//!
//! Extraction compiles every extension file, evaluates it in a throwaway
//! context and reads the registry back. A session then loads that exact
//! bytecode into the VM the user's run owns. Anything the two contexts
//! disagree about — which globals exist, which operator ceiling applies,
//! which files have already evaluated, what a module is named — is a
//! package that passes `ferridriver ext check` and does nothing at
//! runtime, or the reverse.

use std::path::PathBuf;
use std::sync::Arc;

use ferridriver_script::{
  ExtensionBinding, InMemoryVars, Outcome, RunContext, RunOptions, ScriptEngineConfig, Session,
  compile_and_extract_extensions,
};

fn policy() -> ferridriver_config::ExtensionPolicyConfig {
  ferridriver_config::ExtensionPolicyConfig::default()
}

async fn compile(files: &[PathBuf]) -> (Vec<ferridriver_script::CompiledExtension>, Vec<(PathBuf, String)>) {
  let (ok, err) =
    compile_and_extract_extensions(&files.iter().map(|f| vec![f.clone()]).collect::<Vec<_>>(), &policy()).await;
  (ok, err.into_iter().map(|(p, e)| (p, e.message)).collect())
}

fn binding(cp: &ferridriver_script::CompiledExtension) -> ExtensionBinding {
  ExtensionBinding {
    bytecode: cp.bytecode.clone(),
    name: cp.path.display().to_string(),
    source_map: None,
    provides: None,
  }
}

async fn session_with(extensions: Vec<ExtensionBinding>) -> (tempfile::TempDir, Session, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  (tmp, session, ctx)
}

async fn eval(session: &Session, ctx: &RunContext, code: &str) -> serde_json::Value {
  let run = session.execute(code, &[], RunOptions::default(), ctx).await;
  match run.result.outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("execute failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_extensions_compiled_in_separate_batches_both_register() {
  // Every load path compiles whatever is cold in ITS batch, so two
  // extensions routinely reach one session having been compiled apart —
  // from different runs, or from a disk-cache hit alongside a cold file.
  // Both are then named after their position, so both are
  // `ferri_extension_0.js`, and this pins that QuickJS keeps them as two
  // distinct modules anyway: a bytecode module is created by the load,
  // not looked up by name. The name is only a lookup key for import
  // resolution and for `call_site::register_bundle`, which dedupes
  // source maps by it — so the day extensions register one, the name has
  // to come from the file rather than the batch position.
  let tmp = tempfile::tempdir().expect("tempdir");
  let alpha = tmp.path().join("alpha.js");
  let beta = tmp.path().join("beta.js");
  std::fs::write(
    &alpha,
    "defineTool({ name: 'alpha', handler: async () => ({ from: 'alpha' }) });",
  )
  .expect("write");
  std::fs::write(
    &beta,
    "defineTool({ name: 'beta', handler: async () => ({ from: 'beta' }) });",
  )
  .expect("write");

  let (a, a_err) = compile(std::slice::from_ref(&alpha)).await;
  assert!(a_err.is_empty(), "alpha failed: {a_err:?}");
  let (b, b_err) = compile(std::slice::from_ref(&beta)).await;
  assert!(b_err.is_empty(), "beta failed: {b_err:?}");

  let (_tmp, session, ctx) = session_with(vec![binding(&a[0]), binding(&b[0])]).await;
  let names = eval(&session, &ctx, "return Object.keys(tools).sort().join(',');").await;
  let names = names.as_str().unwrap_or_default().to_string();
  assert!(names.contains("alpha"), "alpha missing from `{names}`");
  assert!(names.contains("beta"), "beta missing from `{names}`");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_extension_may_use_the_test_surface_at_its_top_level() {
  // `install_test` runs for every host in a session, so an extension is
  // entitled to build a fixture chain at its top level. If extraction
  // does not install it, the file throws there and is skipped with a
  // warning — it never reaches the session that would have run it.
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("uses-test.ts");
  std::fs::write(
    &path,
    "import { test, expect } from '@ferridriver/test';\n\
     const withUser = test.extend<{ user: string }>({ user: ['guest', { option: true }] });\n\
     defineTool({ name: 'shapes', handler: async () => ({\n\
     \x20 test: typeof test, extended: typeof withUser.extend, expect: typeof expect }) });\n",
  )
  .expect("write");

  let (ok, err) = compile(std::slice::from_ref(&path)).await;
  assert!(err.is_empty(), "extraction failed: {err:?}");
  assert!(
    ok[0].manifests_json().contains("shapes"),
    "manifest missing the tool: {}",
    ok[0].manifests_json()
  );

  let (_tmp, session, ctx) = session_with(vec![binding(&ok[0])]).await;
  let shapes = eval(&session, &ctx, "return await tools.shapes({});").await;
  assert_eq!(
    shapes,
    serde_json::json!({ "test": "function", "extended": "function", "expect": "function" }),
    "the session saw a different test surface than extraction"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cached_file_still_evaluates_before_a_cold_one() {
  // Extraction shares ONE context across the batch, and a session shares
  // one VM: a file that has already evaluated is visible to the next.
  // Skipping the evaluation of cache HITS breaks that — the cold file is
  // extracted in a context the warm one never ran in, which is exactly
  // the provider / consumer order package-owned specifiers depend on.
  let tmp = tempfile::tempdir().expect("tempdir");
  let provider = tmp.path().join("provider.js");
  let consumer = tmp.path().join("consumer.js");
  std::fs::write(&provider, "globalThis.__provided = 'from-provider';").expect("write");
  std::fs::write(
    &consumer,
    "defineTool({ name: 'reads', handler: async () => ({ saw: globalThis.__provided ?? 'nothing' }) });\n\
     if (globalThis.__provided !== 'from-provider') { throw new Error('provider had not evaluated'); }\n",
  )
  .expect("write");

  let files = vec![provider.clone(), consumer.clone()];
  let (_ok, err) = compile(&files).await;
  assert!(err.is_empty(), "cold batch failed: {err:?}");

  // Only the consumer changes: the provider is now a cache hit and the
  // consumer a miss, the arrangement every incremental run produces.
  std::fs::write(
    &consumer,
    "defineTool({ name: 'reads', handler: async () => ({ saw: globalThis.__provided ?? 'nothing' }) });\n\
     if (globalThis.__provided !== 'from-provider') { throw new Error('provider had not evaluated'); }\n\
     defineTool({ name: 'reads2', handler: async () => ({ ok: true }) });\n",
  )
  .expect("write");

  let (ok, err) = compile(&files).await;
  assert!(err.is_empty(), "warm batch failed: {err:?}");
  assert_eq!(ok.len(), 2, "both files must survive");
}

#[tokio::test(flavor = "multi_thread")]
async fn extraction_enforces_the_operator_command_ceiling() {
  // `defineTool` clamps `allow.commands` against the operator ceiling at
  // REGISTRATION time. Extraction that carries no ceiling accepts a
  // package the session then refuses, so `ferridriver ext check` reports
  // a package that cannot load.
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("shelly.js");
  std::fs::write(
    &path,
    "defineTool({ name: 'shelly', allow: { commands: { build: 'sh -c \"echo hi\"' } }, \
     handler: async () => ({ ok: true }) });",
  )
  .expect("write");

  let strict = ferridriver_config::ExtensionPolicyConfig {
    commands: ferridriver_config::ExtensionCommandsCeiling::None,
    ..ferridriver_config::ExtensionPolicyConfig::default()
  };
  let (ok, err) = compile_and_extract_extensions(&[vec![path.clone()]], &strict).await;
  assert!(ok.is_empty(), "the ceiling must refuse the file");
  assert!(
    err.iter().any(|(_, e)| e.message.contains("allow.commands")),
    "expected the ceiling's message, got: {:?}",
    err.iter().map(|(_, e)| &e.message).collect::<Vec<_>>()
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_native_module_instance_serves_every_file_in_a_vm() {
  // The mechanism package-owned import specifiers rest on: a specifier
  // marked external resolves to ONE module instance per VM, and its
  // export slots hold VALUES copied when the module evaluated. Reassigning
  // the property the module read from therefore cannot reach an importer.
  //
  // That is why a fixture-set change has to mutate the one `test` object
  // in place (`defineFixtures`) rather than replace it, and why
  // `ferridriver.test` is read-only: an assignment that looks like it
  // replaced the base chain while reaching nobody is the worst of the
  // three outcomes. `mergeTests` is the writable neighbour that shows
  // what such an assignment actually achieves.
  let tmp = tempfile::tempdir().expect("tempdir");
  let first = tmp.path().join("first.ts");
  let second = tmp.path().join("second.ts");
  std::fs::write(
    &first,
    "import { test, mergeTests } from '@ferridriver/test';\n\
     globalThis.__firstTest = test;\n\
     globalThis.__firstMerge = mergeTests;\n\
     globalThis.__rebindThrew = false;\n\
     try { ferridriver.test = function fake() {}; } catch (e) { globalThis.__rebindThrew = true; }\n\
     ferridriver.mergeTests = function fakeMerge() {};\n",
  )
  .expect("write");
  std::fs::write(
    &second,
    "import { test, mergeTests } from '@ferridriver/test';\n\
     defineTool({ name: 'identity', handler: async () => ({\n\
     \x20 sameInstance: globalThis.__firstTest === test,\n\
     \x20 rebindThrew: globalThis.__rebindThrew,\n\
     \x20 testStillTheBase: ferridriver.test === test,\n\
     \x20 reassignReachedTheImport: ferridriver.mergeTests === mergeTests,\n\
     \x20 importStillTheOriginal: globalThis.__firstMerge === mergeTests }) });\n",
  )
  .expect("write");

  let (ok, err) = compile(&[first.clone(), second.clone()]).await;
  assert!(err.is_empty(), "extraction failed: {err:?}");

  let (_tmp, session, ctx) = session_with(ok.iter().map(binding).collect()).await;
  let facts = eval(&session, &ctx, "return await tools.identity({});").await;
  assert_eq!(
    facts["sameInstance"],
    serde_json::json!(true),
    "two files must import the same module instance: {facts}"
  );
  assert_eq!(
    facts["rebindThrew"],
    serde_json::json!(true),
    "assigning to the read-only `ferridriver.test` must be refused outright: {facts}"
  );
  assert_eq!(
    facts["testStillTheBase"],
    serde_json::json!(true),
    "the refused assignment must leave the base test object in place: {facts}"
  );
  assert_eq!(
    facts["reassignReachedTheImport"],
    serde_json::json!(false),
    "an export slot holds a copied value, so reassigning the source cannot reach an importer: {facts}"
  );
  assert_eq!(
    facts["importStillTheOriginal"],
    serde_json::json!(true),
    "every importer keeps the value the module exported: {facts}"
  );
}
