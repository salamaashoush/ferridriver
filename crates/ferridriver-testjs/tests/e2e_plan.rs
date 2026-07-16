#![allow(clippy::expect_used, clippy::unwrap_used)]
//! End-to-end: `.test.ts` files -> `build_ts_plan` -> core
//! `TestRunner::run` with a real headless browser — the full
//! `ferridriver test` execution path minus the CLI.

use ferridriver_test::config::{CliOverrides, ReporterConfig, TestConfig};
use ferridriver_test::runner::TestRunner;
use ferridriver_testjs::build_ts_plan;

fn config_for(dir: &std::path::Path) -> TestConfig {
  let mut config = TestConfig {
    test_dir: Some(dir.display().to_string()),
    test_match: vec!["**/*.test.ts".to_string()],
    workers: 1,
    output_dir: dir.join("test-results"),
    reporter: vec![ReporterConfig {
      name: "null".to_string(),
      options: std::collections::BTreeMap::new(),
    }],
    ..TestConfig::default()
  };
  config.browser.headless = true;
  config
}

async fn run_plan(dir: &std::path::Path) -> i32 {
  let config = config_for(dir);
  let (plan, sessions) = build_ts_plan(&config, dir)
    .await
    .expect("plan builds")
    .expect("files discovered");
  let code = TestRunner::new(config, CliOverrides::default()).run(plan).await;
  sessions.teardown().await;
  code
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ts_plan_runs_green_through_the_core_runner() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("suite.test.ts"),
    r"import { test, describe, expect } from '@ferridriver/test';

globalThis.order = [];

test.beforeEach(async () => {
  globalThis.order.push('beforeEach');
});

test('navigates and asserts', async ({ page, browserName }) => {
  globalThis.order.push('body');
  await page.goto('data:text/html,<title>E2E</title><h1 id=h>hello</h1>');
  await expect(page).toHaveTitle('E2E');
  await expect(page.locator('#h')).toHaveText('hello');
  if (typeof browserName !== 'string') throw new Error('browserName missing');
});

describe.serial('steps and info', () => {
  test('step returns value and testInfo works', async ({ page, testInfo }) => {
    const title = await test.step('navigate', async () => {
      await page.goto('data:text/html,<title>Steps</title>');
      return page.title();
    });
    if (title !== 'Steps') throw new Error('step return: ' + title);
    if (!testInfo.title.includes('testInfo works')) throw new Error('title: ' + testInfo.title);
    await testInfo.attach('note', 'text/plain', 'attached', undefined);
    if (testInfo.attachmentCount !== 1) throw new Error('attachmentCount');
  });
});

const extended = test.extend({
  greeting: async ({ browserName }, use) => {
    await use('hi ' + browserName);
  },
});

extended('custom fixture', async ({ greeting }) => {
  if (!greeting.startsWith('hi ')) throw new Error('greeting: ' + greeting);
});

test('runtime skip is not a failure', async () => {
  test.skip(true, 'demonstrates skip');
  throw new Error('unreachable');
});

test('expected failure inverts', async () => {
  test.fail();
  throw new Error('meant to fail');
});

test('zz hook probe', async () => {
  const order = globalThis.order;
  if (!order.includes('beforeEach')) throw new Error('beforeEach never ran: ' + JSON.stringify(order));
  if (order.indexOf('beforeEach') > order.indexOf('body')) throw new Error('hook after body: ' + JSON.stringify(order));
});
",
  )
  .expect("write suite");

  let code = Box::pin(run_plan(dir.path())).await;
  assert_eq!(code, 0, "runner must be green");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failing_body_fails_the_run_with_mapped_location() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    dir.path().join("red.test.ts"),
    r"import { test } from '@ferridriver/test';

test('passes', async ({ page }) => {
  await page.goto('data:text/html,<title>ok</title>');
});

test('fails', async () => {
  throw new Error('deliberate red');
});
",
  )
  .expect("write suite");

  let code = Box::pin(run_plan(dir.path())).await;
  assert_ne!(code, 0, "a failing test must fail the run");
}
