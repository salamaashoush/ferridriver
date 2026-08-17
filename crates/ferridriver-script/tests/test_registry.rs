#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Registration surface of `@ferridriver/test`: a bundled `.ts` test
//! module evaluates in an `ExtensionHost::Test` session and every
//! `test`/`describe`/modifier/hook/use/extend call lands in the Rust
//! `TestRegistry`, snapshotted via `collect_tests`.

use std::sync::Arc;

use ferridriver_script::{
  CollectedTests, ExtensionHost, InMemoryVars, PathSandbox, RunContext, ScriptCaps, ScriptEngineConfig, Session,
  bundle_and_compile_named, collect_tests, eval_bundle,
};

/// Collects formatted tracing output so a test can assert on a named
/// diagnostic the way an operator would read it.
#[derive(Clone)]
struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for CapturedLogs {
  fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
    self.0.lock().expect("logs").extend_from_slice(buf);
    Ok(buf.len())
  }

  fn flush(&mut self) -> std::io::Result<()> {
    Ok(())
  }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
  type Writer = Self;

  fn make_writer(&'a self) -> Self::Writer {
    self.clone()
  }
}

/// The subscriber has to be the GLOBAL one: the diagnostic is emitted on
/// the session's VM thread, which a thread-local `set_default` in the
/// test's own thread would never see.
fn captured_logs() -> &'static CapturedLogs {
  static LOGS: std::sync::OnceLock<CapturedLogs> = std::sync::OnceLock::new();
  LOGS.get_or_init(|| {
    let logs = CapturedLogs(Arc::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
      .with_writer(logs.clone())
      .with_ansi(false)
      .with_max_level(tracing::Level::WARN)
      .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
    logs
  })
}

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
    host: ExtensionHost::Test,
    caps: ScriptCaps::default(),
    session: None,
  }
}

async fn collect_from(
  dir: &std::path::Path,
  entry: &std::path::Path,
) -> (CollectedTests, ferridriver_script::CompiledBundle) {
  let bundle = bundle_and_compile_named(std::slice::from_ref(&entry.to_path_buf()), dir, "ferridriver-tests.js")
    .await
    .expect("bundle");
  let context = ctx(dir);
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  eval_bundle(&session.vm_handle(), &bundle).await.expect("eval bundle");
  let collected = collect_tests(&session.vm_handle()).await.expect("collect");
  (collected, bundle)
}

#[tokio::test(flavor = "multi_thread")]
async fn registration_shapes_and_source_mapped_locations() {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("shapes.test.ts");
  std::fs::write(
    &entry,
    r"import { test, describe, expect } from '@ferridriver/test';

if (typeof test !== 'function') throw new Error('test is not a function');
if (typeof describe !== 'function') throw new Error('describe is not a function');
if (typeof expect !== 'function') throw new Error('expect is not a function');

test('plain test', async ({ page, context }) => {
  await page.goto('about:blank');
});

test('with details', { tag: ['smoke', 'fast'], annotation: { type: 'issue', description: 'JIRA-1' }, timeout: 1234, retries: 2 }, async ({ request }) => {});

test.skip('registration skip', async ({ page }) => {});
test.fixme('registration fixme', () => {});
test.fail('registration fail', async ({ page }) => {});
test.slow('registration slow', async ({ page }) => {});

describe('outer', () => {
  test.use({ locale: 'de-DE' });
  test('inner test', async ({ page, testInfo }) => {});
  describe.serial('nested serial', () => {
    test('serial child', async () => {});
  });
});

describe.skip('skipped suite', () => {
  test('inside skipped', () => {});
});

test.use({ colorScheme: 'dark' });

test.beforeEach(async ({ page }) => {});
test.afterAll(() => {});

test.each([
  { name: 'Alice', n: 1 },
  { name: 'Bob', n: 2 },
])('greets $name', async ({ page }, row) => {});

const extended = test.extend({
  greeting: async ({ browserName }, use) => {
    await use('hi');
  },
  port: [4321, { option: true }],
});

extended('uses custom fixture', async ({ page, greeting, port }) => {});
",
  )
  .expect("write entry");

  let (c, bundle) = collect_from(dir.path(), &entry).await;
  assert_registration_shapes(&c);
  assert_fixtures_and_locations(&c, &bundle);
}

fn assert_registration_shapes(c: &CollectedTests) {
  let titles: Vec<&str> = c.tests.iter().map(|t| t.title.as_str()).collect();
  assert_eq!(
    titles,
    [
      "plain test",
      "with details",
      "registration skip",
      "registration fixme",
      "registration fail",
      "registration slow",
      "inner test",
      "serial child",
      "inside skipped",
      "greets Alice",
      "greets Bob",
      "uses custom fixture",
    ]
  );
  assert!(!c.has_only);

  // Fixture inference off the destructured body params.
  let plain = &c.tests[0];
  assert_eq!(
    plain.requested.as_deref(),
    Some(["page".to_string(), "context".to_string()].as_slice())
  );
  let fixme = &c.tests[3];
  assert_eq!(
    fixme.requested.as_deref(),
    Some([].as_slice()),
    "() => {{}} has no keys"
  );

  // TestDetails: tags + info annotation + timeout/retries.
  let details = &c.tests[1];
  let kinds: Vec<(&str, Option<&str>)> = details
    .annotations
    .iter()
    .map(|a| (a.kind.as_str(), a.value.as_deref()))
    .collect();
  assert_eq!(
    kinds,
    [("tag", Some("smoke")), ("tag", Some("fast")), ("info", Some("issue")),]
  );
  assert_eq!(details.annotations[2].description.as_deref(), Some("JIRA-1"));
  assert_eq!(details.timeout_ms, Some(1234));
  assert_eq!(details.retries, Some(2));

  // Registration modifiers annotate.
  for (idx, kind) in [(2usize, "skip"), (3, "fixme"), (4, "fail"), (5, "slow")] {
    assert_eq!(c.tests[idx].annotations.len(), 1, "test {idx}");
    assert_eq!(c.tests[idx].annotations[0].kind, kind);
  }

  // Suite tree: outer -> nested serial; skipped suite annotated.
  assert_eq!(c.suites.len(), 3);
  assert_eq!(c.suites[0].name, "outer");
  assert_eq!(c.suites[0].parent, None);
  assert_eq!(
    c.suites[0].use_options,
    Some(serde_json::json!({ "locale": "de-DE" })),
    "describe-scoped test.use lands on the suite"
  );
  assert_eq!(c.suites[1].name, "nested serial");
  assert_eq!(c.suites[1].parent, Some(0));
  assert_eq!(c.suites[1].mode.as_deref(), Some("serial"));
  assert_eq!(c.suites[2].name, "skipped suite");
  assert_eq!(c.suites[2].annotations[0].kind, "skip");

  // Test-suite membership.
  assert_eq!(c.tests[6].suite, Some(0), "inner test in outer");
  assert_eq!(c.tests[7].suite, Some(1), "serial child in nested");
  assert_eq!(c.tests[8].suite, Some(2));
  assert_eq!(c.tests[0].suite, None);

  // File-scope test.use travels with a location.
  assert_eq!(c.file_use.len(), 1);
  assert_eq!(c.file_use[0].options, serde_json::json!({ "colorScheme": "dark" }));
  assert!(c.file_use[0].line > 0);

  // Hooks with suite association (both registered at file scope here).
  assert_eq!(c.hooks.len(), 2);
  assert_eq!(c.hooks[0].kind, "beforeEach");
  assert_eq!(c.hooks[0].suite, None);
  assert_eq!(c.hooks[0].requested.as_deref(), Some(["page".to_string()].as_slice()));
  assert_eq!(c.hooks[1].kind, "afterAll");

  // test.each rows expand with interpolated titles and carry the row.
  assert!(c.tests[9].has_each_arg && c.tests[10].has_each_arg);
  assert!(!c.tests[0].has_each_arg);
}

fn assert_fixtures_and_locations(c: &CollectedTests, bundle: &ferridriver_script::CompiledBundle) {
  // test.extend: new fixture set with both entries, visible to the test.
  assert_eq!(c.fixtures.len(), 2);
  assert_eq!(c.fixtures[0].name, "greeting");
  assert_eq!(
    c.fixtures[0].deps,
    ["browserName".to_string()],
    "factory deps come from the destructured first param only (use is the second param)"
  );
  assert_eq!(c.fixtures[1].name, "port");
  assert!(c.fixtures[1].option);
  assert_eq!(c.fixture_sets.len(), 2);
  assert_eq!(c.fixture_sets[1], [0, 1]);
  assert_eq!(c.tests[11].fixture_set, 1);
  assert_eq!(c.tests[0].fixture_set, 0);

  // Location capture: every registration has a bundled position that
  // remaps to the original source file.
  for t in &c.tests {
    assert!(t.line > 0, "test `{}` has no location", t.title);
    let (src, line, _col) = bundle
      .remap(t.line, t.col)
      .unwrap_or_else(|| panic!("test `{}` at {}:{} does not remap", t.title, t.line, t.col));
    assert!(src.ends_with("shapes.test.ts"), "test `{}` remaps to {src}", t.title);
    assert!(line > 0);
  }

  // Registration order follows source order per file: line numbers of
  // top-level tests are strictly increasing.
  let lines: Vec<u32> = c
    .tests
    .iter()
    .map(|t| bundle.remap(t.line, t.col).expect("remap").1)
    .collect();
  let mut sorted = lines.clone();
  sorted.sort_unstable();
  assert_eq!(lines, sorted, "registration order matches source order");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_and_configure_and_describe_each() {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("only.test.ts");
  std::fs::write(
    &entry,
    r"import { test, describe } from '@ferridriver/test';

test.only('focused', async ({ page }) => {});

describe('configured', () => {
  describe.configure({ mode: 'serial', retries: 3, timeout: 9000 });
  test('in configured', () => {});
});

describe.each([{ backend: 'cdp' }, { backend: 'webkit' }])('on $backend', (row) => {
  test('per-backend test', () => {});
});
",
  )
  .expect("write entry");

  let (c, _bundle) = collect_from(dir.path(), &entry).await;

  assert!(c.has_only);
  assert_eq!(c.tests[0].annotations[0].kind, "only");

  let configured = &c.suites[0];
  assert_eq!(configured.name, "configured");
  assert_eq!(configured.mode.as_deref(), Some("serial"));
  assert_eq!(configured.retries, Some(3));
  assert_eq!(configured.timeout_ms, Some(9000));

  let each_suites: Vec<&str> = c.suites[1..].iter().map(|s| s.name.as_str()).collect();
  assert_eq!(each_suites, ["on cdp", "on webkit"]);
  assert_eq!(c.tests[2].suite, Some(1));
  assert_eq!(c.tests[3].suite, Some(2));
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_modifiers_outside_a_test_are_hard_errors() {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("bad.test.ts");
  std::fs::write(&entry, "import { test } from '@ferridriver/test';\ntest.skip();\n").expect("write entry");

  let bundle = bundle_and_compile_named(&[entry], dir.path(), "ferridriver-tests.js")
    .await
    .expect("bundle");
  let context = ctx(dir.path());
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  let err = eval_bundle(&session.vm_handle(), &bundle)
    .await
    .expect_err("top-level runtime skip must fail");
  assert!(
    err.message.contains("can only be called while a test is running"),
    "unexpected error: {}",
    err.message
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_test_hosts_expose_the_whole_surface() {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("probe.test.ts");
  std::fs::write(
    &entry,
    "import { test, describe, expect, mergeTests } from '@ferridriver/test';\n\
     for (const [name, value] of Object.entries({ test, describe, expect, mergeTests })) {\n\
       if (typeof value !== 'function') throw new Error(`${name} missing under the script host`);\n\
     }\n\
     const a = test.extend({ a: async ({}, use) => { await use(1); } });\n\
     const b = test.extend({ b: async ({}, use) => { await use(2); } });\n\
     if (typeof mergeTests(a, b).extend !== 'function') throw new Error('mergeTests chain is not a test');\n",
  )
  .expect("write entry");

  let bundle = bundle_and_compile_named(&[entry], dir.path(), "ferridriver-tests.js")
    .await
    .expect("bundle");
  for host in [ExtensionHost::Script, ExtensionHost::Bdd, ExtensionHost::Mcp] {
    let mut context = ctx(dir.path());
    context.host = host;
    let session = Session::create(ScriptEngineConfig::default(), &context)
      .await
      .expect("session");
    eval_bundle(&session.vm_handle(), &bundle)
      .await
      .unwrap_or_else(|e| panic!("the test surface must exist under {host:?}: {e:?}"));
  }
}

/// Registering a test where nothing collects it is inert, so it is named
/// rather than silently kept: the surface stays usable (an extension
/// builds fixture chains with it) but the call is reported.
#[tokio::test(flavor = "multi_thread")]
async fn a_test_registered_off_the_test_host_is_reported() {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("probe.test.ts");
  std::fs::write(
    &entry,
    "import { test } from '@ferridriver/test';\n\
     test('inert', async () => {});\n",
  )
  .expect("write entry");

  let bundle = bundle_and_compile_named(&[entry], dir.path(), "ferridriver-tests.js")
    .await
    .expect("bundle");
  let mut context = ctx(dir.path());
  context.host = ExtensionHost::Script;
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");

  let logs = captured_logs().clone();

  eval_bundle(&session.vm_handle(), &bundle)
    .await
    .expect("registration under a non-test host is not an error");

  let written = logs.0.lock().expect("logs").clone();
  let written = String::from_utf8_lossy(&written);
  assert!(
    written.contains("test.registration.ignored"),
    "expected the named diagnostic, got: {written}"
  );
}
