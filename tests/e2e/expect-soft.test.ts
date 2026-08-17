// A soft assertion is recorded and the test carries on, failing at the
// end. Runs on every backend project: the recording path goes through
// the per-test host bridge, and a soft web-first matcher polls a live
// page before it records.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('expect.soft', () => {
  test('records a value failure and keeps going', async ({ testInfo }) => {
    // The recorded failure fails this test at the end, which is what
    // `test.fail()` expects here.
    test.fail();
    const before = testInfo.errors.length;
    expect.soft(1).toBe(2);
    // Execution continued past the failed assertion — a hard one would
    // have thrown before this line.
    const reached = true;
    expect(reached).toBe(true);
    expect(testInfo.errors.length).toBe(before + 1);
    expect(testInfo.errors[before]).toContain('toBe');
  });

  test('records every soft failure, not just the first', async ({ testInfo }) => {
    test.fail();
    const before = testInfo.errors.length;
    expect.soft(1).toBe(2);
    expect.soft('a').toBe('b');
    expect.soft([1]).toHaveLength(9);
    expect(testInfo.errors.length).toBe(before + 3);
  });

  test('a passing soft assertion records nothing', async ({ testInfo }) => {
    const before = testInfo.errors.length;
    expect.soft(1).toBe(1);
    expect.soft('a').not.toBe('b');
    expect(testInfo.errors.length).toBe(before);
  });

  test('a soft web-first matcher records after its timeout', async ({ page, testInfo }) => {
    test.fail();
    await page.goto(dataUrl("<h1 id='t'>here</h1>"));
    const before = testInfo.errors.length;
    const started = Date.now();
    await expect.soft(page.locator('#missing')).toBeVisible({ timeout: 400 });
    // It polled and then recorded instead of throwing.
    expect(Date.now() - started).toBeGreaterThanOrEqual(300);
    expect(testInfo.errors.length).toBe(before + 1);
    expect(testInfo.errors[before]).toContain('toBeVisible');
    // The test is still running, so the page is still usable.
    await expect(page.locator('#t')).toHaveText('here');
  });

  test('configure({ soft: true }) makes every assertion soft', async ({ testInfo }) => {
    test.fail();
    const soft = expect.configure({ soft: true });
    const before = testInfo.errors.length;
    soft(1).toBe(2);
    soft(2).toBe(3);
    expect(testInfo.errors.length).toBe(before + 2);
  });

  test('a soft custom matcher records too', async ({ testInfo }) => {
    test.fail();
    const withMatcher = expect.extend({
      toBeAlpha(received: string) {
        return { pass: received === 'a', message: () => `not alpha: ${received}` };
      },
    });
    const before = testInfo.errors.length;
    withMatcher.soft('z').toBeAlpha();
    expect(testInfo.errors.length).toBe(before + 1);
    expect(testInfo.errors[before]).toContain('not alpha: z');
  });

  test('a hard assertion still throws while soft ones are pending', async ({ testInfo }) => {
    test.fail();
    expect.soft(1).toBe(2);
    let threw = false;
    try {
      expect(3).toBe(4);
    } catch {
      threw = true;
    }
    expect(threw).toBe(true);
    expect(testInfo.errors.length).toBeGreaterThanOrEqual(1);
  });
});
