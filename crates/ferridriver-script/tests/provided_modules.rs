#![allow(clippy::expect_used, clippy::unwrap_used)]
//! A package serves an import specifier, and everything that imports it
//! gets ONE module.
//!
//! This is the mechanism the whole extension system exists for: a suite
//! written against some other package imports its specifier and
//! resolves to the package ferridriver loaded, with no edit to the
//! suite's own source. It only works if the specifier stays external to
//! every bundle and links, at load, against the single module the
//! provider's bytecode already is — two consumers each inlining their
//! own copy would give the "shared" module two states.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ferridriver_script::{
  ExtensionSpec, InMemoryVars, Outcome, PathSandbox, RequirementEnv, RunContext, RunOptions, ScriptCaps,
  ScriptEngineConfig, Session,
};

/// ONE package for the whole binary.
///
/// The claim table is process-global and seals on first use, which is
/// the production invariant: a session created before a specifier
/// arrives would resolve without it for life. So every test here shares
/// one table — and the last test pins what happens to a claim that
/// arrives after the seal.
fn shared_package() -> &'static Path {
  static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
  DIR.get_or_init(|| {
    let dir = scratch("shared");
    write_package(&dir);
    dir
  })
}

fn shared_specs() -> Vec<ExtensionSpec> {
  vec![ExtensionSpec {
    spec: "./pkg".to_string(),
    base_dir: shared_package().to_path_buf(),
  }]
}

fn scratch(tag: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |d| d.as_nanos());
  let dir = std::env::temp_dir().join(format!("ferri_provided_{tag}_{nanos}"));
  std::fs::create_dir_all(&dir).expect("mkdir");
  dir
}

/// A package that serves `fake-vendor` from a provider holding mutable
/// state, plus two independent entry files that both import it.
fn write_package(dir: &Path) {
  std::fs::create_dir_all(dir.join("pkg")).expect("mkdir pkg");
  std::fs::write(
    dir.join("pkg/package.json"),
    r#"{"name":"vendor-pkg","ferridriver":{
        "apiVersion":2,
        "name":"vendor-pkg",
        "entries":["one.ts","two.ts","a.ts","b.ts"],
        "provides":{"modules":{"fake-vendor":"vendor.ts"},"aliases":{"fake-vendor/alias":"fake-vendor"}}
      }}"#,
  )
  .expect("write package.json");
  std::fs::write(
    dir.join("pkg/vendor.ts"),
    "export const marker = { seen: [] as string[] };\nexport function note(who: string) { marker.seen.push(who); }\n",
  )
  .expect("write provider");
  std::fs::write(
    dir.join("pkg/one.ts"),
    "import { marker, note } from 'fake-vendor';\n\
     note('one');\n\
     (globalThis as Record<string, unknown>).__one = marker;\n",
  )
  .expect("write entry one");
  std::fs::write(
    dir.join("pkg/state.ts"),
    "export const state = { entries: [] as string[] };\n",
  )
  .expect("write helper");
  std::fs::write(
    dir.join("pkg/a.ts"),
    "import { state } from './state.ts';\nstate.entries.push('a');\n(globalThis as Record<string, unknown>).__stateA = state;\n",
  )
  .expect("write entry a");
  std::fs::write(
    dir.join("pkg/b.ts"),
    "import { state } from './state.ts';\nstate.entries.push('b');\n(globalThis as Record<string, unknown>).__stateB = state;\n",
  )
  .expect("write entry b");
  std::fs::write(
    dir.join("pkg/two.ts"),
    "import { marker } from 'fake-vendor/alias';\n\
     (globalThis as Record<string, unknown>).__two = marker;\n",
  )
  .expect("write entry two");
}

async fn session_for(dir: &Path, specs: &[ExtensionSpec]) -> (Session, RunContext, Vec<String>) {
  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  let (gated, _compiled, failures) = ferridriver_script::extension_load::load(
    specs,
    &env,
    &caps.extension_policy,
    ferridriver_script::ExtensionHost::Script,
  )
  .await;
  assert!(failures.is_empty(), "compile failures: {failures:?}");
  let diagnostics: Vec<String> = gated.issues.iter().map(|i| i.message.clone()).collect();
  let bindings = ferridriver_script::load_bindings(
    specs,
    &env,
    &caps.extension_policy,
    ferridriver_script::ExtensionHost::Script,
  )
  .await;
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: bindings,
    host: ferridriver_script::ExtensionHost::Script,
    caps,
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  (session, ctx, diagnostics)
}

async fn eval(session: &Session, ctx: &RunContext, code: &str) -> serde_json::Value {
  match session
    .execute(code, &[], RunOptions::default(), ctx)
    .await
    .result
    .outcome
  {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("execute failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn two_bundles_importing_a_claimed_specifier_share_one_module() {
  let dir = shared_package();
  let specs = shared_specs();
  let (session, ctx, diagnostics) = session_for(dir, &specs).await;
  assert!(
    diagnostics.iter().all(|d| !d.contains("claim")),
    "no claim should be refused: {diagnostics:?}"
  );

  // Identity, not equality: two independently-bundled entries received
  // the same object, and one entry's mutation is visible in the other.
  let facts = eval(
    &session,
    &ctx,
    "return { same: globalThis.__one === globalThis.__two, seen: globalThis.__one.seen };",
  )
  .await;
  assert_eq!(
    facts["same"],
    serde_json::json!(true),
    "both entries must receive ONE module instance: {facts}"
  );
  assert_eq!(
    facts["seen"],
    serde_json::json!(["one"]),
    "the mutation one entry made must be visible through the other's import: {facts}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claimed_specifier_is_importable_from_a_plain_script() {
  // Not only from an extension: the point of a claim is that ordinary
  // code — a spec, a step file, a `run` script — imports the specifier.
  let dir = shared_package();
  let specs = shared_specs();
  let (session, ctx, _diagnostics) = session_for(dir, &specs).await;

  let entry = dir.join("consumer.ts");
  std::fs::write(
    &entry,
    "import { marker, note } from 'fake-vendor';\nnote('script');\nexport default marker.seen.join(',');\n",
  )
  .expect("write consumer");
  let bundle = ferridriver_script::bundle_and_compile(std::slice::from_ref(&entry), dir)
    .await
    .expect("bundle consumer");
  let run = session.execute_module(&bundle, &[], RunOptions::default(), &ctx).await;
  match run.result.outcome {
    // The provider already recorded `one`; the script appends to the
    // SAME array, which is what proves it did not get a fresh copy.
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("one,script")),
    Outcome::Error { error } => panic!("consumer module failed: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn require_answers_a_claimed_specifier_with_the_same_module() {
  // `require` is synchronous and cannot await a dynamic import, so it
  // reads the namespace remembered when the provider evaluated. If it
  // built its own object instead, a CommonJS consumer would mutate a
  // copy nobody else sees.
  let dir = shared_package();
  let specs = shared_specs();
  let (session, ctx, _diagnostics) = session_for(dir, &specs).await;

  let facts = eval(
    &session,
    &ctx,
    "const v = require('fake-vendor');\n\
     const a = require('fake-vendor/alias');\n\
     v.note('require');\n\
     return { same: v.marker === a.marker, live: v.marker === globalThis.__one, seen: v.marker.seen };",
  )
  .await;
  assert_eq!(
    facts["same"],
    serde_json::json!(true),
    "an alias requires the same module: {facts}"
  );
  assert_eq!(
    facts["live"],
    serde_json::json!(true),
    "require and import must answer with ONE object: {facts}"
  );
  assert!(
    facts["seen"]
      .as_array()
      .is_some_and(|s| s.contains(&serde_json::json!("require"))),
    "the mutation must land on the shared module: {facts}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claim_arriving_after_the_table_is_sealed_is_refused() {
  // The table seals the first time anything resolves against it. A
  // session created before a specifier arrived would keep a resolver
  // that never heard of it, and a bundle keyed before it would have
  // inlined what should have stayed external — so a late claim is an
  // error rather than a table that means two things.
  let dir = scratch("late");
  std::fs::create_dir_all(dir.join("pkg")).expect("mkdir pkg");
  std::fs::write(
    dir.join("pkg/package.json"),
    r#"{"name":"late-pkg","ferridriver":{"entries":["e.ts"],"provides":{"modules":{"late-vendor":"v.ts"}}}}"#,
  )
  .expect("write package.json");
  std::fs::write(dir.join("pkg/v.ts"), "export const late = 1;\n").expect("write provider");
  std::fs::write(dir.join("pkg/e.ts"), "export {};\n").expect("write entry");

  // Make sure the shared table is installed and sealed first.
  let _ = session_for(shared_package(), &shared_specs()).await;

  let caps = ScriptCaps::default();
  let sidecars: Vec<String> = Vec::new();
  let env = RequirementEnv::from_caps(&caps, &sidecars);
  let specs = vec![ExtensionSpec {
    spec: "./pkg".to_string(),
    base_dir: dir.clone(),
  }];
  let (gated, _compiled, _failures) = ferridriver_script::extension_load::load(
    &specs,
    &env,
    &caps.extension_policy,
    ferridriver_script::ExtensionHost::Script,
  )
  .await;
  assert!(
    gated
      .issues
      .iter()
      .any(|i| i.message.contains("sealed") && i.message.contains("late-vendor")),
    "the late claim must be reported: {:?}",
    gated.issues.iter().map(|i| &i.message).collect::<Vec<_>>()
  );
  assert!(
    ferridriver_script::provided_modules::canonical_provided_name("late-vendor").is_none(),
    "and it must not be resolvable"
  );

  let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_entries_of_one_package_share_their_helper() {
  // A package's entries are ONE bundle. Bundling them apart inlines a
  // shared helper into each, so the array the package keeps in that
  // helper holds one name in each copy instead of both in one — the
  // module is "shared" in the source and duplicated in the run.
  let dir = shared_package();
  let specs = shared_specs();
  let (session, ctx, _diagnostics) = session_for(dir, &specs).await;
  let facts = eval(
    &session,
    &ctx,
    "return { same: globalThis.__stateA === globalThis.__stateB, entries: globalThis.__stateA.entries };",
  )
  .await;
  assert_eq!(
    facts["same"],
    serde_json::json!(true),
    "both entries must import ONE helper module: {facts}"
  );
  assert_eq!(
    facts["entries"],
    serde_json::json!(["a", "b"]),
    "each entry appends to the same array: {facts}"
  );
}
