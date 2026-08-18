import { test, expect } from '@ferridriver/test';

// `test.skip(callback, description)` at file or describe scope —
// Playwright's suite modifiers (`common/testType.ts:232`, evaluated in
// `worker/workerMain.ts:545`). The callback receives that test's
// fixtures, so ONE call decides a whole group per browser.
//
// The proof has to be a test that would fail if it ran, plus a sibling
// that runs and observes the group's other half was skipped. A test
// that merely passes proves nothing about a skip.

const ran: string[] = [];

test.describe('skipped on chromium', () => {
  test.skip(({ browserName }) => browserName === 'chromium', 'chromium only');

  test('does not run on chromium', async ({ browserName }) => {
    ran.push('chromium-guarded');
    expect(browserName).not.toBe('chromium');
  });
});

test.describe('skipped everywhere', () => {
  test.skip(() => true, 'always');

  test('never runs', async () => {
    throw new Error('a truthy callback modifier must skip this body');
  });
});

test.describe('skipped nowhere', () => {
  test.skip(() => false, 'never');

  test('always runs', async ({ browserName }) => {
    expect(typeof browserName).toBe('string');
  });
});

test.describe('the callback sees custom fixtures', () => {
  const guarded = test.extend<{ engine: string }>({
    engine: async ({ browserName }, use) => {
      await use(browserName === 'webkit' ? 'jsc' : 'other');
    },
  });

  guarded.skip(({ engine }) => engine === 'jsc', 'not on JavaScriptCore');

  guarded('does not run on webkit', async ({ browserName }) => {
    expect(browserName).not.toBe('webkit');
  });
});

test.describe('fail and slow take callbacks too', () => {
  test.fail(({ browserName }) => browserName.length > 0, 'always expected to fail');

  test('is expected to fail', async () => {
    throw new Error('this failure is the expected outcome');
  });
});

test.describe('nesting', () => {
  test.skip(() => false, 'outer says no');

  test.describe('inner', () => {
    test.skip(() => true, 'inner says yes');

    test('inner is skipped', async () => {
      throw new Error('the inner modifier must win');
    });
  });

  test('outer still runs', async () => {
    expect(true).toBe(true);
  });
});

test.describe('scope errors', () => {
  test('a callback inside a body is refused', async () => {
    let message = 'no-throw';
    try {
      // The callback overload type-checks anywhere — it is the RUNTIME
      // that refuses it here, because a test body's fixtures are already
      // resolved and a callback could no longer change the outcome.
      test.skip(() => true, 'wrong scope');
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message).toContain('can only be called inside describe block');
  });

  test('the static form still works inside a body', async ({ browserName }) => {
    // The condition form is unchanged by the callback support.
    test.skip(browserName === 'no-such-browser', 'never true');
    expect(browserName.length).toBeGreaterThan(0);
  });
});
