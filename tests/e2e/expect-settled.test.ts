// `.resolves` / `.rejects` settle the subject and then run the ordinary
// matcher against what it settled to. Runs on every backend project: the
// chain is built by reflecting over the Expect prototype, so a matcher
// reaching a live page through it has to work on each of them.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

async function rejection(fn: () => Promise<unknown>): Promise<string> {
  try {
    await fn();
  } catch (e) {
    return String((e as Error).message ?? e);
  }
  return '';
}

describe('expect settled', () => {
  test('resolves runs the matcher on the resolved value', async () => {
    await expect(Promise.resolve(7)).resolves.toBe(7);
    await expect(Promise.resolve({ a: 1 })).resolves.toEqual({ a: 1 });
    await expect(Promise.resolve('hello world')).resolves.toContain('world');
    await expect(Promise.resolve(7)).resolves.not.toBe(8);
    // A function returning a promise is accepted too.
    await expect(async () => 'later').resolves.toBe('later');
  });

  test('rejects runs the matcher on the rejection reason', async () => {
    await expect(Promise.reject(new Error('boom'))).rejects.toThrow('boom');
    await expect(Promise.reject(new RangeError('out'))).rejects.toThrow(RangeError);
    await expect(Promise.reject(new Error('boom'))).rejects.not.toThrow('other');
    // The reason itself is the subject for the value matchers.
    await expect(Promise.reject('plain')).rejects.toBe('plain');
  });

  test('settling the wrong way says which way', async () => {
    const a = await rejection(() => expect(Promise.reject(new Error('x'))).resolves.toBe(1));
    expect(a).toContain('rejected instead of resolved');
    const b = await rejection(() => expect(Promise.resolve(1)).rejects.toBe(1));
    expect(b).toContain('resolved instead of rejected');
  });

  test('a settled chain needs a promise', async () => {
    const msg = await rejection(() => expect(1).resolves.toBe(1));
    expect(msg).toContain('promise, or a function returning a promise');
  });

  test('a settled matcher fails like any other', async () => {
    const msg = await rejection(() => expect(Promise.resolve(1)).resolves.toBe(2));
    expect(msg).toContain('toBe');
  });

  test('expect.poll refuses a settled chain', async () => {
    const msg = await rejection(async () => (expect.poll(() => 1) as unknown as { resolves: unknown }).resolves);
    expect(msg).toContain('does not support');
  });

  test('a promise resolving to a page handle gets that handle’s matchers', async ({ page }) => {
    await page.goto(dataUrl("<h1 id='t'>settled</h1>"));
    // The settled value is dispatched afresh, so the Locator matchers —
    // not just the value ones — are reachable through the chain.
    await expect(Promise.resolve(page.locator('#t'))).resolves.toHaveText('settled');
    await expect(Promise.resolve(page.locator('#missing'))).resolves.toHaveCount(0);
    // And a real page-driven promise settles the same way.
    await expect(page.textContent('#t')).resolves.toBe('settled');
  });
});
