// Ported from Playwright's tests/playwright-test/expect-poll.spec.ts:
// expect.poll takes every generic matcher and, on timeout, reports the
// last failure with the predicate timeout in its call log.
import { test, expect } from '@ferridriver/test';

test.describe('expect.poll', () => {
  test('polls sync, async and promise-returning predicates', async () => {
    let i = 0;
    await expect.poll(() => ++i).toBe(3);
    let j = 0;
    await expect.poll(async () => {
      await new Promise(x => setTimeout(x, 50));
      return ++j;
    }).toBe(3);
    let k = 0;
    await expect.poll(() => Promise.resolve(++k)).toBe(3);
  });

  test('takes any generic matcher', async () => {
    let i = 0;
    await expect.poll(() => ++i).toBeGreaterThan(2);
    const items: string[] = [];
    await expect.poll(() => { items.push('x'); return items; }).toContain('x');
    await expect.poll(() => 'hello world').toMatch(/world/);
    await expect.poll(() => ({ a: 1, b: { c: 2 } })).toMatchObject({ b: { c: 2 } });
  });

  test('supports .not', async () => {
    let i = 0;
    await expect.poll(() => ++i).not.toBeLessThan(3);
    expect(i).toBe(3);
  });

  test('reports the last failure with the predicate timeout', async () => {
    const error = await expect.poll(() => false, { timeout: 100 }).toBe(3).catch((e: Error) => e) as Error;
    expect(error.message).toContain('Timeout 100ms exceeded while waiting on the predicate');
    expect(error.message).toContain('Received: false');
  });
});
