#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `defineFixtures` — an extension package contributing onto the BASE
//! fixture chain, so a suite that never imports the package still
//! receives the fixtures through its own `test`.
//!
//! The mechanism is the whole risk. The base chain cannot be REPLACED:
//! `@ferridriver/test` is one module instance per VM whose export slots
//! hold values copied when it evaluated, so an importer keeps the `test`
//! object it linked against however `ferridriver.test` is reassigned
//! afterwards (pinned in `extraction_context.rs`). `defineFixtures`
//! therefore appends to fixture set 0 in place, and that set seals once
//! the last extension has installed — because from then on every
//! `test.extend()` COPIES it.

use std::path::PathBuf;
use std::sync::Arc;

use ferridriver_config::ExtensionPolicyConfig;
use ferridriver_script::{
  CollectedTests, CompiledBundle, ExtensionBinding, ExtensionHost, InMemoryVars, Outcome, PathSandbox, RunContext,
  RunOptions, ScriptCaps, ScriptEngineConfig, Session, bundle_and_compile_named, collect_tests,
  compile_and_extract_extensions, eval_bundle, run_test,
};
use ferridriver_test::host::{RunTestSpec, TestInfoData, TestWorldData};

mod support;

use support::MockBridge;

fn open_policy() -> ExtensionPolicyConfig {
  ExtensionPolicyConfig::default()
}

fn closed_policy() -> ExtensionPolicyConfig {
  ExtensionPolicyConfig {
    fixtures: false,
    ..ExtensionPolicyConfig::default()
  }
}

/// Compile each source as its own extension file, in the order given —
/// which is the order the session installs them in, and therefore the
/// order their contributions compose in.
async fn extensions_from(
  dir: &std::path::Path,
  sources: &[&str],
  policy: &ExtensionPolicyConfig,
) -> Vec<ExtensionBinding> {
  let mut groups: Vec<Vec<PathBuf>> = Vec::new();
  for (i, src) in sources.iter().enumerate() {
    let path = dir.join(format!("ext{i}.ts"));
    std::fs::write(&path, src).expect("write extension");
    groups.push(vec![path]);
  }
  let (compiled, failures) = compile_and_extract_extensions(&groups, policy).await;
  assert!(
    failures.is_empty(),
    "compile failures: {:?}",
    failures
      .iter()
      .map(|(p, e)| (p.clone(), e.message.clone()))
      .collect::<Vec<_>>()
  );
  compiled
    .into_iter()
    .map(|cp| ExtensionBinding {
      bytecode: cp.bytecode,
      name: cp.path.display().to_string(),
      source_map: None,
      provides: None,
    })
    .collect()
}

fn run_context(dir: &std::path::Path, extensions: Vec<ExtensionBinding>, policy: ExtensionPolicyConfig) -> RunContext {
  RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions,
    host: ExtensionHost::Test,
    caps: ScriptCaps::default().with_extension_policy(policy),
    session: None,
  }
}

struct Harness {
  session: Session,
  collected: CollectedTests,
  _bundle: CompiledBundle,
}

/// A session carrying `extensions`, with `spec` bundled and evaluated in
/// it. The spec never imports the extensions — it imports
/// `@ferridriver/test` and nothing else, which is the point.
async fn harness(extensions: &[&str], spec: &str, policy: &ExtensionPolicyConfig) -> Harness {
  let dir = tempfile::tempdir().expect("tempdir");
  let bindings = extensions_from(dir.path(), extensions, policy).await;
  let entry = dir.path().join("contributed.test.ts");
  std::fs::write(&entry, spec).expect("write spec");
  let bundle = bundle_and_compile_named(&[entry], dir.path(), "ferridriver-tests.js")
    .await
    .expect("bundle");
  let context = run_context(dir.path(), bindings, policy.clone());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session create");
  eval_bundle(&session.vm_handle(), &bundle).await.expect("eval bundle");
  let collected = collect_tests(&session.vm_handle()).await.expect("collect");
  // The tempdir must outlive bundling only; leak it so the files behind
  // the disk cache stay valid for the session's life.
  std::mem::forget(dir);
  Harness {
    session,
    collected,
    _bundle: bundle,
  }
}

fn world(title: &str) -> TestWorldData {
  TestWorldData {
    page: None,
    context: None,
    request: None,
    browser: None,
    browser_name: "chromium".to_string(),
    headless: true,
    is_mobile: false,
    has_touch: false,
    base_url: None,
    use_options: serde_json::json!({}),
    expect: Arc::default(),
    info: TestInfoData {
      title: title.to_string(),
      title_path: vec![title.to_string()],
      file: "contributed.test.ts".to_string(),
      line: 1,
      column: 1,
      retry: 0,
      worker_index: 0,
      parallel_index: 0,
      repeat_each_index: 0,
      timeout_ms: 30_000,
      expected_status: "passed".to_string(),
      tags: Vec::new(),
      output_dir: "/out".to_string(),
      snapshot_dir: "/snap".to_string(),
      snapshot_suffix: String::new(),
      project_name: Some("unit".to_string()),
    },
  }
}

fn spec_for(collected: &CollectedTests, title: &str) -> RunTestSpec {
  RunTestSpec {
    test_idx: collected
      .tests
      .iter()
      .position(|t| t.title == title)
      .unwrap_or_else(|| panic!("no test titled `{title}`")),
    modifiers: Vec::new(),
    hooks_before: Vec::new(),
    hooks_after: Vec::new(),
    source_label: "contributed.test.ts".to_string(),
  }
}

async fn run_body(h: &Harness, title: &str) -> Result<(), ferridriver_script::ScriptError> {
  run_test(
    &h.session.vm_handle(),
    spec_for(&h.collected, title),
    world(title),
    Arc::new(MockBridge::default()),
  )
  .await
}

async fn eval_err(session: &Session, ctx: &RunContext, code: &str) -> String {
  match session
    .execute(code, &[], RunOptions::default(), ctx)
    .await
    .result
    .outcome
  {
    Outcome::Error { error } => error.message,
    Outcome::Ok { success } => panic!("expected a failure, got: {:?}", success.value),
  }
}

/// A spec that only passes when both packages' contributions reached the
/// base chain AND the later package's `label` shadowed the earlier one
/// while resolving it as its own `super`.
const SPEC: &str = "
import { test } from '@ferridriver/test';

test('receives the contributed fixtures', async ({ label, onlyFirst }) => {
  if (label !== 'first+second') throw new Error('label: ' + String(label));
  if (onlyFirst !== 'kept') throw new Error('onlyFirst: ' + String(onlyFirst));
});
";

const FIRST: &str = "
defineFixtures({
  label: async ({}, use) => { await use('first'); },
  onlyFirst: async ({}, use) => { await use('kept'); },
});
";

const SECOND: &str = "
defineFixtures({
  label: async ({ label }, use) => { await use(label + '+second'); },
});
";

/// Two packages compose in load order: the later one shadows the
/// earlier, and its same-name dependency resolves to the registration it
/// shadows — `test.extend`'s super rule, which is what makes a
/// contribution an override rather than a cycle.
#[tokio::test(flavor = "multi_thread")]
async fn two_packages_compose_in_load_order_with_the_later_one_shadowing() {
  let h = harness(&[FIRST, SECOND], SPEC, &open_policy()).await;
  run_body(&h, "receives the contributed fixtures")
    .await
    .expect("the spec must see both contributions, with the later `label` on top");
}

/// The inversion: the SAME spec, with nothing contributing. Both
/// fixtures are then unknown names, so the body cannot pass by accident.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_packages_the_same_spec_fails() {
  let h = harness(&[], SPEC, &open_policy()).await;
  let err = run_body(&h, "receives the contributed fixtures")
    .await
    .expect_err("nothing else can supply `label`");
  assert!(
    err.message.contains("label"),
    "the failure must name the missing fixture, got: {}",
    err.message
  );
}

/// Contribution order is package order, not name order: swapping the two
/// packages swaps which one is on top.
#[tokio::test(flavor = "multi_thread")]
async fn swapping_the_packages_swaps_which_contribution_wins() {
  const REVERSED_SPEC: &str = "
import { test } from '@ferridriver/test';

test('later package wins', async ({ label }) => {
  if (label !== 'first') throw new Error('label: ' + String(label));
});
";
  // SECOND installs first, so its `{ label }` dependency has no earlier
  // registration to resolve and falls through; FIRST then registers on
  // top and its plain value is what the spec sees.
  let h = harness(&[SECOND, FIRST], REVERSED_SPEC, &open_policy()).await;
  run_body(&h, "later package wins")
    .await
    .expect("the last package to install owns the name");
}

/// An `auto` contribution runs for a spec that names nothing at all —
/// the shape a package uses to install cross-cutting setup.
#[tokio::test(flavor = "multi_thread")]
async fn an_auto_contribution_runs_without_the_spec_naming_it() {
  const AUTO: &str = "
defineFixtures({
  marker: [async ({}, use) => { globalThis.__autoRan = (globalThis.__autoRan ?? 0) + 1; await use(1); }, { auto: true }],
});
";
  const AUTO_SPEC: &str = "
import { test } from '@ferridriver/test';

test('auto ran', async () => {
  if (globalThis.__autoRan !== 1) throw new Error('autoRan: ' + String(globalThis.__autoRan));
});
";
  let h = harness(&[AUTO], AUTO_SPEC, &open_policy()).await;
  run_body(&h, "auto ran")
    .await
    .expect("an auto contribution runs for every test");
}

/// Once every extension has installed, the base chain is sealed: a
/// `defineFixtures` from a spec bundle, a step file or a `run_script`
/// throws the documented message instead of reaching the suites that
/// have not derived a chain yet and missing the ones that have.
#[tokio::test(flavor = "multi_thread")]
async fn define_fixtures_after_the_extensions_are_installed_throws() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = run_context(dir.path(), Vec::new(), open_policy());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session create");
  let err = eval_err(
    &session,
    &context,
    "defineFixtures({ late: async ({}, use) => { await use(1); } });",
  )
  .await;
  assert!(
    err.contains("defineFixtures() can only be called while an extension is loading") && err.contains("test.extend()"),
    "expected the sealed-chain message pointing at the alternative, got: {err}"
  );
}

/// `[extensions.policy] fixtures = false` refuses the contribution,
/// names the key, and fails the session rather than skipping the file —
/// a run that continued without the package would be a deployment the
/// operator never agreed to.
#[tokio::test(flavor = "multi_thread")]
async fn the_fixtures_ceiling_refuses_the_contribution_naming_the_key() {
  let dir = tempfile::tempdir().expect("tempdir");
  // Compiled under the open policy, refused under the closed one — the
  // ceiling is the operator's, applied where the package actually runs.
  let bindings = extensions_from(dir.path(), &[FIRST], &open_policy()).await;
  let context = run_context(dir.path(), bindings, closed_policy());
  let err = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .err()
    .expect("the ceiling must refuse the session");
  assert!(
    err.message.contains("extension.policy.refused") && err.message.contains("[extensions.policy] fixtures = false"),
    "the refusal must name itself and the key, got: {}",
    err.message
  );
}

/// The ceiling covers the `defineFixtures` entry point ONLY. A package
/// building its own chain with `test.extend` / `mergeTests`, or adding a
/// matcher with `expect.extend`, changes nothing a suite did not ask for
/// by importing it; clamping those would be a different and unannounced
/// policy.
#[tokio::test(flavor = "multi_thread")]
async fn the_fixtures_ceiling_never_clamps_extend_merge_or_expect_extend() {
  const OWN_CHAIN: &str = "
const a = ferridriver.test.extend({ a: async ({}, use) => { await use('a'); } });
const b = ferridriver.test.extend({ b: async ({}, use) => { await use('b'); } });
const merged = ferridriver.mergeTests(a, b);
expect.extend({ toBeFortyTwo(received) { return { pass: received === 42, message: () => 'nope' }; } });
expect(42).toBeFortyTwo();
globalThis.__ownChain = typeof merged === 'function';
";
  const OWN_SPEC: &str = "
import { test } from '@ferridriver/test';

test('own chains survive the ceiling', async () => {
  if (globalThis.__ownChain !== true) throw new Error('ownChain: ' + String(globalThis.__ownChain));
});
";
  let h = harness(&[OWN_CHAIN], OWN_SPEC, &closed_policy()).await;
  run_body(&h, "own chains survive the ceiling")
    .await
    .expect("a closed fixtures ceiling must not touch test.extend / mergeTests / expect.extend");
}

/// `ferridriver.test` is read-only. A package that reassigned it would
/// look like it had replaced the base chain while every module that
/// already imported kept the original — the exact failure
/// `defineFixtures` exists to avoid.
#[tokio::test(flavor = "multi_thread")]
async fn the_base_test_object_cannot_be_reassigned() {
  let dir = tempfile::tempdir().expect("tempdir");
  let context = run_context(dir.path(), Vec::new(), open_policy());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session create");
  let outcome = session
    .execute(
      "'use strict';\nferridriver.test = function fake() {};\nreturn 'assigned';",
      &[],
      RunOptions::default(),
      &context,
    )
    .await
    .result
    .outcome;
  match outcome {
    Outcome::Error { error } => assert!(
      error.message.contains("read-only") || error.message.contains("read only"),
      "expected a read-only assignment failure, got: {}",
      error.message
    ),
    Outcome::Ok { success } => panic!("the assignment must be refused, got: {:?}", success.value),
  }
}

/// Manifest extraction sees the contribution: the extraction pass
/// installs the same test surface and the same ceiling, so `ferridriver
/// ext check` reports the fixtures a session would really register.
#[tokio::test(flavor = "multi_thread")]
async fn extraction_reports_contributed_fixtures_under_every_host() {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("contrib.ts");
  std::fs::write(&path, FIRST).expect("write");

  let (compiled, failures) = compile_and_extract_extensions(&[vec![path]], &open_policy()).await;
  assert!(failures.is_empty(), "extraction failed: {failures:?}");
  let snapshot = &compiled.first().expect("one extension").snapshot;
  for host in ["mcp", "bdd", "test", "script"] {
    let registrations = snapshot.for_host(host).unwrap_or_else(|| panic!("no slice for {host}"));
    assert!(
      registrations.fixtures.iter().any(|f| f == "onlyFirst"),
      "host {host} must report the contributed fixture, got: {:?}",
      registrations.fixtures
    );
  }
}

/// And extraction enforces the same ceiling, so `ext check` cannot
/// report a package the session then refuses.
#[tokio::test(flavor = "multi_thread")]
async fn extraction_enforces_the_fixtures_ceiling() {
  let tmp = tempfile::tempdir().expect("tempdir");
  let path = tmp.path().join("contrib.ts");
  std::fs::write(&path, FIRST).expect("write");

  let (ok, err) = compile_and_extract_extensions(&[vec![path]], &closed_policy()).await;
  assert!(ok.is_empty(), "the ceiling must refuse the file");
  assert!(
    err
      .iter()
      .any(|(_, e)| e.message.contains("[extensions.policy] fixtures = false")),
    "expected the ceiling's message, got: {:?}",
    err.iter().map(|(_, e)| &e.message).collect::<Vec<_>>()
  );
}

/// Both entry points are importable from the `ferridriver` module, not
/// only reachable as globals — which is how a package with a tsconfig
/// that forbids implicit globals writes them.
#[tokio::test(flavor = "multi_thread")]
async fn the_module_exports_both_entry_points() {
  const IMPORTED: &str = "
import { defineFixtures, bindSteps, test, mergeTests } from 'ferridriver';
import { test as fromTestModule } from '@ferridriver/test';

if (test !== fromTestModule) { throw new Error('the two surfaces must hand back one test object'); }
if (typeof mergeTests !== 'function') { throw new Error('mergeTests'); }

defineFixtures({ viaImport: async ({}, use) => { await use('imported'); } });
const { Given } = bindSteps(test);
Given('the import worked', async ({ viaImport }) => { globalThis.__seen = viaImport; });
";
  const IMPORTED_SPEC: &str = "
import { test } from '@ferridriver/test';

test('the imported entry point contributed', async ({ viaImport }) => {
  if (viaImport !== 'imported') throw new Error('viaImport: ' + String(viaImport));
});
";
  let h = harness(&[IMPORTED], IMPORTED_SPEC, &open_policy()).await;
  run_body(&h, "the imported entry point contributed")
    .await
    .expect("defineFixtures and bindSteps must both be importable from `ferridriver`");
}

/// A deployment that never mentions the key keeps working.
#[test]
fn the_policy_key_defaults_open() {
  assert!(ExtensionPolicyConfig::default().fixtures);
}
