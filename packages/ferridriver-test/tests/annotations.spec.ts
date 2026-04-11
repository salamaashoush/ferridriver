import { test, describe } from '../src/index.js';

// ── Runtime skip (Playwright API) ──

test('runtime skip on firefox', async ({ page, browserName }) => {
  test.skip(browserName === 'firefox', 'not supported on firefox');
  await page.goto('data:text/html,<title>Not Firefox</title>');
  const title = await page.title();
  if (title !== 'Not Firefox') throw new Error(`unexpected: ${title}`);
});

test('runtime skip unconditional', async ({ page }) => {
  test.skip();
  throw new Error('should not reach here');
});

// ── Registration-time skip ──

test.skip('registration skip', async ({ page }) => {
  throw new Error('should not reach here');
});

// ── Runtime fixme ──

test('runtime fixme', async ({ page, browserName }) => {
  test.fixme(browserName === 'webkit', 'known webkit bug');
  await page.goto('data:text/html,<title>OK</title>');
});

// ── Registration-time fixme ──

test.fixme('registration fixme', async ({ page }) => {
  throw new Error('should not reach here');
});

// ── Runtime fail ──

test('runtime fail inverts result', async ({ page }) => {
  test.fail();
  throw new Error('intentional failure — inverted to pass');
});

test('runtime fail conditional', async ({ page, browserName }) => {
  test.fail(browserName === 'webkit', 'expected to fail on webkit');
  await page.goto('data:text/html,<title>OK</title>');
  const title = await page.title();
  if (title !== 'OK') throw new Error(`unexpected: ${title}`);
});

// ── Registration-time fail ──

test.fail('registration fail', async ({ page }) => {
  throw new Error('intentional failure — inverted to pass');
});

// ── Runtime slow ──

test('runtime slow', async ({ page }) => {
  test.slow();
  await page.goto('data:text/html,<title>Slow</title>');
});

// ── Registration-time slow ──

test.slow('registration slow', async ({ page }) => {
  await page.goto('data:text/html,<title>Slow</title>');
});

// ── test.info() ──

test('test info accessible', async ({ page, testInfo }) => {
  if (!testInfo.title) throw new Error('testInfo.title is empty');
  if (typeof testInfo.retry !== 'number') throw new Error('testInfo.retry is not a number');
  if (typeof testInfo.workerIndex !== 'number') throw new Error('testInfo.workerIndex is not a number');
  await page.goto('data:text/html,<title>Info</title>');
});

// ── Fixtures ──

test('all fixtures available', async ({ page, browserName, headless, isMobile, hasTouch, context, request }) => {
  if (typeof browserName !== 'string') throw new Error('browserName not a string');
  if (typeof headless !== 'boolean') throw new Error('headless not a boolean');
  if (typeof isMobile !== 'boolean') throw new Error('isMobile not a boolean');
  if (typeof hasTouch !== 'boolean') throw new Error('hasTouch not a boolean');
  if (!context) throw new Error('context is null');
  if (!request) throw new Error('request is null');
  await page.goto('data:text/html,<title>Fixtures</title>');
});

// ── describe.skip ──

describe.skip('skipped describe', () => {
  test('inside skipped describe', async () => {
    throw new Error('should not run');
  });
});

// ── describe.fixme ──

describe.fixme('fixme describe', () => {
  test('inside fixme describe', async () => {
    throw new Error('should not run');
  });
});

// ── describe.serial ──

describe.serial('serial suite', () => {
  test('serial test 1', async ({ page }) => {
    await page.goto('data:text/html,<title>Serial1</title>');
  });
  test('serial test 2', async ({ page }) => {
    await page.goto('data:text/html,<title>Serial2</title>');
  });
});

// ── test(name, details, body) — TestDetails ──

test('test with details', { tag: ['smoke', 'fast'], annotation: { type: 'issue', description: 'JIRA-123' } }, async ({ page }) => {
  await page.goto('data:text/html,<title>Details</title>');
});

// ── test.step() ──

test('test.step creates structured steps', async ({ page }) => {
  await test.step('navigate', async () => {
    await page.goto('data:text/html,<title>Steps</title>');
  });
  await test.step('verify title', async () => {
    const title = await page.title();
    if (title !== 'Steps') throw new Error(`unexpected: ${title}`);
  });
});

test('test.step returns value', async ({ page }) => {
  const title = await test.step('get title', async () => {
    await page.goto('data:text/html,<title>ReturnVal</title>');
    return await page.title();
  });
  if (title !== 'ReturnVal') throw new Error(`expected ReturnVal, got ${title}`);
});

// ── test.use() ──

describe('test.use scope', () => {
  test.use({ locale: 'de-DE', colorScheme: 'dark' });

  test('use options are set', async ({ page }) => {
    // test.use() sets options that the worker applies to the context.
    // For now we just verify the test runs without error.
    await page.goto('data:text/html,<title>UseOptions</title>');
  });
});

// ── test.setTimeout() ──

test('test.setTimeout adjusts timeout', async ({ page, testInfo }) => {
  test.setTimeout(60000);
  await page.goto('data:text/html,<title>Timeout</title>');
});

// ── test.info() ──

test('test.info returns testInfo', async ({ page }) => {
  const info = test.info();
  if (!info) throw new Error('test.info() returned null');
  if (!info.title) throw new Error('test.info().title is empty');
  if (typeof info.workerIndex !== 'number') throw new Error('workerIndex not a number');
});

// ── Hooks ──
// Note: beforeEach/afterEach hooks require suite registration wiring.
// TODO: test once hook-to-suite association is implemented.

// ── test.each ──

test.each([
  { name: 'Alice', greeting: 'Hello Alice' },
  { name: 'Bob', greeting: 'Hello Bob' },
])('greeting for $name', async ({ page }, { name, greeting }) => {
  await page.goto(`data:text/html,<title>${greeting}</title>`);
  const title = await page.title();
  if (title !== greeting) throw new Error(`expected ${greeting}, got ${title}`);
});
