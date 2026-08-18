#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Invocation surface of `@ferridriver/test`: registered bodies run
//! through `run_test` with a mock [`TestHostBridge`] — fixtures object,
//! custom-fixture use()-handshake lifecycle, each-hooks, runtime
//! modifiers, `test.step`, and the `testInfo` object.

use std::sync::Arc;

use ferridriver_script::{
  CollectedTests, CompiledBundle, ExtensionHost, InMemoryVars, PathSandbox, RunContext, ScriptCaps, ScriptEngineConfig,
  Session, TEST_SKIP_SENTINEL, bundle_and_compile_named, collect_tests, eval_bundle, run_test,
  teardown_worker_fixtures,
};
use ferridriver_test::host::{RunTestSpec, TestInfoData, TestWorldData};

mod support;

use support::MockBridge;

struct Harness {
  session: Session,
  collected: CollectedTests,
  _bundle: CompiledBundle,
}

async fn harness(source: &str) -> Harness {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("invoke.test.ts");
  std::fs::write(&entry, source).expect("write entry");
  let bundle = bundle_and_compile_named(&[entry], dir.path(), "ferridriver-tests.js")
    .await
    .expect("bundle");
  let context = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(dir.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ExtensionHost::Test,
    caps: ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  eval_bundle(&session.vm_handle(), &bundle).await.expect("eval bundle");
  let collected = collect_tests(&session.vm_handle()).await.expect("collect");
  // The tempdir must outlive bundling only; leak it so the fixture
  // files backing the disk cache stay valid for the session's life.
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
    expect: std::sync::Arc::default(),
    info: TestInfoData {
      title: title.to_string(),
      title_path: vec![title.to_string()],
      file: "invoke.test.ts".to_string(),
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

fn spec(idx: usize) -> RunTestSpec {
  RunTestSpec {
    test_idx: idx,
    hooks_before: Vec::new(),
    hooks_after: Vec::new(),
    source_label: "invoke.test.ts".to_string(),
  }
}

fn title_index(c: &CollectedTests, title: &str) -> usize {
  c.tests
    .iter()
    .position(|t| t.title == title)
    .unwrap_or_else(|| panic!("no test titled `{title}`"))
}

#[tokio::test(flavor = "multi_thread")]
async fn fixtures_scalars_and_test_info() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test('scalars', async ({ browserName, headless, isMobile, hasTouch, testInfo }) => {
  if (browserName !== 'chromium') throw new Error('browserName: ' + browserName);
  if (headless !== true) throw new Error('headless');
  if (isMobile !== false) throw new Error('isMobile');
  if (hasTouch !== false) throw new Error('hasTouch');
  if (testInfo.title !== 'scalars') throw new Error('title: ' + testInfo.title);
  if (testInfo.workerIndex !== 0) throw new Error('workerIndex');
  if (testInfo.project.name !== 'unit') throw new Error('project');
  if (testInfo.expectedStatus !== 'passed') throw new Error('expectedStatus');
});

test('second arg is testInfo', async ({ browserName }, info) => {
  if (info.title !== 'second arg is testInfo') throw new Error('info arg: ' + info.title);
});
",
  )
  .await;
  let bridge = Arc::new(MockBridge::default());
  let idx = title_index(&h.collected, "scalars");
  run_test(&h.session.vm_handle(), spec(idx), world("scalars"), bridge.clone())
    .await
    .expect("scalars test passes");
  let idx = title_index(&h.collected, "second arg is testInfo");
  run_test(
    &h.session.vm_handle(),
    spec(idx),
    world("second arg is testInfo"),
    bridge,
  )
  .await
  .expect("info arg test passes");
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_modifiers_reach_the_bridge() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test('skips', async () => {
  test.skip(true, 'not here');
  throw new Error('unreachable');
});

test('fails expectedly', async () => {
  test.fail();
  throw new Error('intended');
});

test('slow and setTimeout', async () => {
  test.slow();
  test.setTimeout(90000);
});

test('conditional skip false path', async ({ browserName }) => {
  test.skip(browserName === 'firefox', 'firefox only');
});
",
  )
  .await;

  let bridge = Arc::new(MockBridge::default());
  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "skips")),
    world("skips"),
    bridge.clone(),
  )
  .await
  .expect_err("skip aborts the body");
  assert!(
    err.message.contains(TEST_SKIP_SENTINEL),
    "sentinel missing: {}",
    err.message
  );
  assert!(bridge.state(|s| s.skipped));
  assert_eq!(bridge.state(|s| s.skip_reason.clone()), Some("not here".to_string()));

  let bridge = Arc::new(MockBridge::default());
  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "fails expectedly")),
    world("fails expectedly"),
    bridge.clone(),
  )
  .await
  .expect_err("body error propagates; worker inverts via expected_failure");
  assert!(err.message.contains("intended"));
  assert!(bridge.state(|s| s.expected_failure));

  let bridge = Arc::new(MockBridge::default());
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "slow and setTimeout")),
    world("slow and setTimeout"),
    bridge.clone(),
  )
  .await
  .expect("modifier-only body passes");
  assert!(bridge.state(|s| s.slow));
  assert_eq!(bridge.state(|s| s.timeout_override), Some(90_000));

  let bridge = Arc::new(MockBridge::default());
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "conditional skip false path")),
    world("conditional skip false path"),
    bridge.clone(),
  )
  .await
  .expect("false condition does not skip");
  assert!(!bridge.state(|s| s.skipped));
}

#[tokio::test(flavor = "multi_thread")]
async fn steps_nest_and_return_values() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test('steps', async () => {
  const v = await test.step('outer', async () => {
    await test.step('inner', async () => {});
    return 42;
  });
  if (v !== 42) throw new Error('step return: ' + v);
});

test('failing step', async () => {
  await test.step('boom', async () => {
    throw new Error('step exploded');
  });
});
",
  )
  .await;

  let bridge = Arc::new(MockBridge::default());
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "steps")),
    world("steps"),
    bridge.clone(),
  )
  .await
  .expect("steps pass");
  let events = bridge.state(|s| s.step_events.clone());
  // Each step also reports where it was opened. The recorder has no
  // source map, so the position is the bundle's own — what matters is
  // that a step is located at all, and that the inner one is a
  // different line from the outer.
  assert_eq!(
    events,
    [
      "begin s1 `outer` parent=- at=bundle.js:4:23",
      "begin s2 `inner` parent=s1 at=bundle.js:5:14",
      "end s2 err=- status=Passed",
      "end s1 err=- status=Passed",
    ]
  );

  let bridge = Arc::new(MockBridge::default());
  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "failing step")),
    world("failing step"),
    bridge.clone(),
  )
  .await
  .expect_err("step error fails the test");
  assert!(err.message.contains("step exploded"), "got: {}", err.message);
  let events = bridge.state(|s| s.step_events.clone());
  assert_eq!(events.len(), 2);
  assert!(events[1].starts_with("end s1 err=") && events[1].contains("step exploded"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_info_attach_annotate_and_getters() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test('attaches', async ({ testInfo }) => {
  await testInfo.attach('positional', 'text/plain', Buffer.from('hello'), undefined);
  await testInfo.attach('bag', { body: 'world', contentType: 'text/x-custom' });
  if (testInfo.attachmentCount !== 2) throw new Error('count: ' + testInfo.attachmentCount);
  testInfo.annotate('issue', 'JIRA-9');
  const anns = testInfo.annotations;
  if (!Array.isArray(anns) || anns.length !== 1) throw new Error('annotations');
  if (anns[0].type !== 'issue' || anns[0].description !== 'JIRA-9') throw new Error('annotation shape');
  if (testInfo.outputPath('a', 'b.txt') !== '/out/a/b.txt') throw new Error('outputPath');
  // The bridge answers `/snap/<kind>/<name>`, so this also pins the
  // default kind Playwright applies when the bag omits it
  // (`worker/testInfo.ts:644`).
  if (testInfo.snapshotPath('x.png') !== '/snap/snapshot/x.png') throw new Error('snapshotPath: ' + testInfo.snapshotPath('x.png'));
  if (testInfo.snapshotPath('x.png', { kind: 'screenshot' }) !== '/snap/screenshot/x.png') throw new Error('snapshotPath kind: ' + testInfo.snapshotPath('x.png', { kind: 'screenshot' }));
});
",
  )
  .await;

  let bridge = Arc::new(MockBridge::default());
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "attaches")),
    world("attaches"),
    bridge.clone(),
  )
  .await
  .expect("attach test passes");
  let atts = bridge.state(|s| s.attachments.clone());
  assert_eq!(atts.len(), 2);
  assert_eq!(atts[0].0, "positional");
  assert_eq!(atts[0].1, "text/plain");
  assert_eq!(atts[0].2, b"hello");
  assert_eq!(atts[1].0, "bag");
  assert_eq!(atts[1].1, "text/x-custom");
  assert_eq!(atts[1].2, b"world");
}

#[tokio::test(flavor = "multi_thread")]
async fn custom_fixtures_setup_use_teardown_lifecycle() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

globalThis.log = [];

const extended = test.extend({
  greeting: async ({ browserName }, use) => {
    globalThis.log.push('setup greeting for ' + browserName);
    await use('hello from ' + browserName);
    globalThis.log.push('teardown greeting');
  },
  port: [4321, { option: true }],
});

extended('uses fixtures', async ({ greeting, port }) => {
  globalThis.log.push('body sees ' + greeting + ' port ' + port);
});

extended('port override', async ({ port }) => {
  globalThis.log.push('override port ' + port);
});

test('log probe', async () => {
  throw new Error('LOG:' + JSON.stringify(globalThis.log));
});
",
  )
  .await;

  let bridge = Arc::new(MockBridge::default());
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "uses fixtures")),
    world("uses fixtures"),
    bridge.clone(),
  )
  .await
  .expect("fixture test passes");

  let mut w = world("port override");
  w.use_options = serde_json::json!({ "port": 9999 });
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "port override")),
    w,
    bridge.clone(),
  )
  .await
  .expect("override test passes");

  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "log probe")),
    world("log probe"),
    bridge,
  )
  .await
  .expect_err("probe throws the log");
  let msg = &err.message;
  assert!(msg.contains("setup greeting for chromium"), "log: {msg}");
  assert!(msg.contains("body sees hello from chromium port 4321"), "log: {msg}");
  assert!(msg.contains("teardown greeting"), "log: {msg}");
  assert!(msg.contains("override port 9999"), "log: {msg}");
  // Teardown ran right after the body, before the next test started.
  let teardown_pos = msg.find("teardown greeting").expect("teardown in log");
  let next_test = msg.find("override port").expect("next test in log");
  assert!(teardown_pos < next_test, "teardown must precede the next test: {msg}");
  assert_eq!(msg.matches("setup greeting").count(), 1, "greeting set up once: {msg}");
}

#[tokio::test(flavor = "multi_thread")]
async fn worker_scoped_fixtures_cache_across_tests() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

globalThis.workerLog = [];

const extended = test.extend({
  server: [
    async ({}, use) => {
      globalThis.workerLog.push('worker setup');
      await use('srv-' + globalThis.workerLog.length);
      globalThis.workerLog.push('worker teardown');
    },
    { scope: 'worker' },
  ],
});

extended('first', async ({ server }) => {
  globalThis.workerLog.push('first got ' + server);
});

extended('second', async ({ server }) => {
  globalThis.workerLog.push('second got ' + server);
});

test('probe', async () => {
  throw new Error('LOG:' + JSON.stringify(globalThis.workerLog));
});
",
  )
  .await;

  let bridge = Arc::new(MockBridge::default());
  for title in ["first", "second"] {
    run_test(
      &h.session.vm_handle(),
      spec(title_index(&h.collected, title)),
      world(title),
      bridge.clone(),
    )
    .await
    .unwrap_or_else(|e| panic!("{title} failed: {}", e.message));
  }
  teardown_worker_fixtures(&h.session.vm_handle())
    .await
    .expect("worker teardown");
  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "probe")),
    world("probe"),
    bridge,
  )
  .await
  .expect_err("probe throws the log");
  let msg = &err.message;
  assert_eq!(msg.matches("worker setup").count(), 1, "one setup: {msg}");
  assert!(msg.contains("first got srv-1"), "log: {msg}");
  assert!(msg.contains("second got srv-1"), "log: {msg}");
  assert_eq!(msg.matches("worker teardown").count(), 1, "one teardown: {msg}");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_hooks_run_around_the_body_and_share_fixtures() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

globalThis.hookLog = [];

test.beforeEach(async ({ browserName }) => {
  globalThis.hookLog.push('before ' + browserName);
});

test.afterEach(async () => {
  globalThis.hookLog.push('after');
});

test('body', async () => {
  globalThis.hookLog.push('body');
});

test('failing body', async () => {
  globalThis.hookLog.push('failing body');
  throw new Error('body failed');
});

test('probe', async () => {
  throw new Error('LOG:' + JSON.stringify(globalThis.hookLog));
});
",
  )
  .await;

  let hooks_before = vec![0usize];
  let hooks_after = vec![1usize];
  let bridge = Arc::new(MockBridge::default());

  let mut s0 = spec(title_index(&h.collected, "body"));
  s0.hooks_before = hooks_before.clone();
  s0.hooks_after = hooks_after.clone();
  run_test(&h.session.vm_handle(), s0, world("body"), bridge.clone())
    .await
    .expect("hooked test passes");

  let mut s1 = spec(title_index(&h.collected, "failing body"));
  s1.hooks_before = hooks_before;
  s1.hooks_after = hooks_after;
  let err = run_test(&h.session.vm_handle(), s1, world("failing body"), bridge.clone())
    .await
    .expect_err("body failure propagates");
  assert!(err.message.contains("body failed"));

  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "probe")),
    world("probe"),
    bridge,
  )
  .await
  .expect_err("probe throws the log");
  let msg = &err.message;
  assert!(
    msg.contains(r#"["before chromium","body","after","before chromium","failing body","after"#),
    "afterEach must run on failure too: {msg}"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_arg_reaches_the_body() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test.each([
  { name: 'Alice', n: 1 },
  { name: 'Bob', n: 2 },
])('row $name', async ({ browserName }, row) => {
  if (typeof row.n !== 'number') throw new Error('row not passed');
  if (row.name === 'Bob' && row.n !== 2) throw new Error('wrong row');
});
",
  )
  .await;
  let bridge = Arc::new(MockBridge::default());
  for title in ["row Alice", "row Bob"] {
    run_test(
      &h.session.vm_handle(),
      spec(title_index(&h.collected, title)),
      world(title),
      bridge.clone(),
    )
    .await
    .unwrap_or_else(|e| panic!("{title} failed: {}", e.message));
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn factory_that_never_calls_use_is_an_error() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

const extended = test.extend({
  broken: async ({}, use) => {
    return 'never used';
  },
});

extended('needs broken', async ({ broken }) => {});
",
  )
  .await;
  let bridge = Arc::new(MockBridge::default());
  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "needs broken")),
    world("needs broken"),
    bridge,
  )
  .await
  .expect_err("factory without use() must fail");
  assert!(
    err.message.contains("finished without calling use()"),
    "got: {}",
    err.message
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_matchers_cross_the_bridge() {
  let h = harness(
    r"import { test, expect } from '@ferridriver/test';

test('value snapshot', async () => {
  await expect('rendered output').toMatchSnapshot('greeting');
  await expect('auto named').toMatchSnapshot();
});
",
  )
  .await;
  let bridge = Arc::new(MockBridge::default());
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "value snapshot")),
    world("value snapshot"),
    bridge.clone(),
  )
  .await
  .expect("snapshot test passes");
  let calls = bridge.state(|s| s.snapshot_calls.clone());
  assert_eq!(calls, ["text value name=greeting", "text value name=<auto>"]);
}

/// A first parameter naming nothing is Playwright's load error, not an
/// `undefined` the body then fails on somewhere unrelated
/// (`common/fixtures.ts:250-256` via `common/poolBuilder.ts:66-71`).
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_first_parameter_name_is_reported_by_name() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test('asks for nothing anyone registered', async ({ deployment }) => {
  if (deployment !== undefined) throw new Error('unreachable');
});
",
  )
  .await;
  let err = run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "asks for nothing anyone registered")),
    world("asks for nothing anyone registered"),
    Arc::new(MockBridge::default()),
  )
  .await
  .expect_err("an unknown parameter must fail the test");
  assert_eq!(err.message, "Test has unknown parameter \"deployment\".");
}

/// A hook is checked too, and the message names the hook the way
/// Playwright does.
#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_hook_parameter_names_the_hook() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test.beforeEach(async ({ notAFixture }) => { void notAFixture; });

test('body is fine', async ({ browserName }) => { void browserName; });
",
  )
  .await;
  let mut s = spec(title_index(&h.collected, "body is fine"));
  s.hooks_before = h
    .collected
    .hooks
    .iter()
    .enumerate()
    .filter(|(_, hook)| hook.kind == "beforeEach")
    .map(|(i, _)| i)
    .collect();
  assert!(!s.hooks_before.is_empty(), "the beforeEach must be collected");
  let err = run_test(
    &h.session.vm_handle(),
    s,
    world("body is fine"),
    Arc::new(MockBridge::default()),
  )
  .await
  .expect_err("an unknown hook parameter must fail the test");
  assert_eq!(err.message, "beforeEach hook has unknown parameter \"notAFixture\".");
}

/// A built-in the world in hand does not carry is still a known name.
/// A `beforeAll` world has no `page`; answering "unknown parameter" for
/// it would be a worse lie than the `undefined` this check replaces.
#[tokio::test(flavor = "multi_thread")]
async fn a_builtin_absent_from_this_world_is_not_an_unknown_parameter() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

test('names a built-in this world lacks', async ({ page }) => {
  if (page !== undefined) throw new Error('this world carries no page');
});
",
  )
  .await;
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "names a built-in this world lacks")),
    world("names a built-in this world lacks"),
    Arc::new(MockBridge::default()),
  )
  .await
  .expect("a built-in name must pass the check whatever this world carries");
}

/// And a registered custom fixture passes, so the check cannot be
/// passing everything or failing everything.
#[tokio::test(flavor = "multi_thread")]
async fn a_registered_fixture_passes_the_check() {
  let h = harness(
    r"import { test } from '@ferridriver/test';

const t = test.extend({ deployment: async ({}, use) => { await use('staging'); } });

t('asks for what it registered', async ({ deployment }) => {
  if (deployment !== 'staging') throw new Error('deployment: ' + String(deployment));
});
",
  )
  .await;
  run_test(
    &h.session.vm_handle(),
    spec(title_index(&h.collected, "asks for what it registered")),
    world("asks for what it registered"),
    Arc::new(MockBridge::default()),
  )
  .await
  .expect("a registered fixture resolves");
}
