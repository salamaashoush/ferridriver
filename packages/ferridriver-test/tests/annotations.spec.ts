import { test, describe } from '../src/index.js';

// ── test.skip() — unconditional ──

test.skip('unconditional skip never runs', async ({ page }) => {
  throw new Error('should not reach here');
});

// ── test.skip via options — conditional ──

test('conditional skip via options — skip on webkit', { skip: 'webkit' }, async ({ page }) => {
  await page.goto('data:text/html,<title>Not WebKit</title>');
  const title = await page.title();
  if (title !== 'Not WebKit') throw new Error(`unexpected title: ${title}`);
});

// ── test.fixme() — unconditional ──

test.fixme('fixme skips like skip', async ({ page }) => {
  throw new Error('should not reach here — fixme skips');
});

// ── test.fixme via options — conditional ──

test('conditional fixme via options', { fixme: 'webkit' }, async ({ page }) => {
  await page.goto('data:text/html,<title>Hello</title>');
  const title = await page.title();
  if (title !== 'Hello') throw new Error(`unexpected title: ${title}`);
});

// ── test.slow() — unconditional ──

test.slow('slow test has tripled timeout', async ({ page }) => {
  await page.goto('data:text/html,<title>Slow</title>');
  const title = await page.title();
  if (title !== 'Slow') throw new Error(`unexpected title: ${title}`);
});

// ── test.slow via options — conditional ──

test('conditional slow via options', { slow: 'ci' }, async ({ page }) => {
  await page.goto('data:text/html,<title>Maybe Slow</title>');
  const title = await page.title();
  if (title !== 'Maybe Slow') throw new Error(`unexpected title: ${title}`);
});

// ── test.fail() — unconditional ──

test.fail('expected failure inverts result', async ({ page }) => {
  // This test deliberately fails — @fail inverts, so it passes.
  await page.goto('data:text/html,<title>Fail</title>');
  const title = await page.title();
  if (title !== 'WRONG TITLE') throw new Error('intentional failure');
});

// ── test.fail via options — conditional ──
// fail: 'webkit' means only invert on webkit. On chromium body must pass.

test('conditional fail via options — pass on chromium', { fail: 'webkit' }, async ({ page }) => {
  await page.goto('data:text/html,<title>OK</title>');
  const title = await page.title();
  if (title !== 'OK') throw new Error(`unexpected title: ${title}`);
});

// ── Options: combined annotations ──

test('multiple annotations via options', { tag: 'smoke', slow: true }, async ({ page }) => {
  await page.goto('data:text/html,<title>Tagged</title>');
  const title = await page.title();
  if (title !== 'Tagged') throw new Error(`unexpected title: ${title}`);
});

// ── env: condition ──

test('env condition skip — runs when env var unset', { skip: 'env:FERRIDRIVER_SKIP_THIS_TEST' }, async ({ page }) => {
  await page.goto('data:text/html,<title>EnvCheck</title>');
  const title = await page.title();
  if (title !== 'EnvCheck') throw new Error(`unexpected title: ${title}`);
});

// ── Negation condition ──

test('negation condition — skip on non-chromium', { skip: '!chromium' }, async ({ page }) => {
  await page.goto('data:text/html,<title>ChromOnly</title>');
  const title = await page.title();
  if (title !== 'ChromOnly') throw new Error(`unexpected title: ${title}`);
});

// ── describe.skip() ──

describe.skip('skipped describe block', () => {
  test('test inside skipped describe', async ({ page }) => {
    throw new Error('should not reach here');
  });
});

// ── describe.fixme() ──

describe.fixme('fixme describe block', () => {
  test('test inside fixme describe', async ({ page }) => {
    throw new Error('should not reach here');
  });
});

// ── describe.slow() ──

describe.slow('slow describe block', () => {
  test('test inside slow describe has tripled timeout', async ({ page }) => {
    await page.goto('data:text/html,<title>SlowBlock</title>');
    const title = await page.title();
    if (title !== 'SlowBlock') throw new Error(`unexpected title: ${title}`);
  });
});

// ── describe.fail() ──

describe.fail('fail describe block', () => {
  test('test inside fail describe expects failure', async ({ page }) => {
    throw new Error('intentional failure — inverted by describe.fail');
  });
});
