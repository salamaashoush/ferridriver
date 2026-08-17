// `expect(...)` keeps the value it was handed alive, so the matchers
// Playwright defines over the value itself — `Object.is`, `instanceof`,
// `[...received]`, `typeof` — mean what they mean upstream. Runs on every
// backend project: the subject rework changes the shared script layer, and
// a Page / Locator receiver has to answer the same six generic matchers on
// each of them.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

function thrown(fn: () => unknown): string {
  try {
    fn();
  } catch (e) {
    return String((e as Error).message ?? e);
  }
  return '';
}

describe('expect subject', () => {
  test('toBe is Object.is, not deep equality', async () => {
    const a = { v: 1 };
    const b = { v: 1 };
    expect(a).toBe(a);
    // Structurally equal, different reference: NOT toBe-equal. A JSON
    // snapshot of the subject cannot tell these apart.
    expect(a).not.toBe(b);
    expect(a).toEqual(b);

    const list = [1, 2];
    expect(list).toBe(list);
    expect(list).not.toBe([1, 2]);

    const failure = thrown(() => expect(a).toBe(b));
    expect(failure).toContain('expect(value).toBe() failed');
    expect(failure).toContain('replace "toBe" with "toEqual"');
  });

  test('toBe follows Object.is on NaN and signed zero', async () => {
    expect(NaN).toBe(NaN);
    expect(0).not.toBe(-0);
    expect(-0).toBe(-0);
  });

  test('a function is a full value subject', async () => {
    const fn = (_a: unknown, _b: unknown) => 42;
    expect(fn).toBe(fn);
    expect(fn).not.toBe((_a: unknown, _b: unknown) => 42);
    expect(fn).toBeDefined();
    expect(fn).toBeTruthy();
    expect(fn).toBeInstanceOf(Function);
    // `.length` is the arity — read off the live function.
    expect(fn).toHaveLength(2);
    // The function-only matchers still work on the same subject.
    expect(() => {
      throw new Error('boom');
    }).toThrow('boom');
  });

  test('undefined and null are distinct subjects', async () => {
    expect(undefined).toBeUndefined();
    expect(undefined).not.toBeNull();
    expect(undefined).not.toBeDefined();
    expect(null).toBeNull();
    expect(null).not.toBeUndefined();
    // `null` IS defined; only `undefined` is not.
    expect(null).toBeDefined();
    expect(undefined).not.toBe(null);
  });

  test('toBeNaN needs a real NaN', async () => {
    expect(NaN).toBeNaN();
    expect(0 / 0).toBeNaN();
    expect('NaN').not.toBeNaN();
    expect(null).not.toBeNaN();
  });

  test('toBeInstanceOf is the instanceof operator', async () => {
    class Base {}
    class Derived extends Base {}
    const d = new Derived();
    expect(d).toBeInstanceOf(Derived);
    // A base class a constructor-NAME comparison would never match.
    expect(d).toBeInstanceOf(Base);
    expect(d).toBeInstanceOf(Object);
    expect(d).not.toBeInstanceOf(Error);
    expect([]).toBeInstanceOf(Array);
    expect(new Error('x')).toBeInstanceOf(Error);

    // A non-function argument is a TypeError, not a failed assertion.
    const msg = thrown(() => expect(d).toBeInstanceOf(5 as unknown as Function));
    expect(msg).toContain('must be a function');
  });

  test('toContain is strict equality over live items', async () => {
    const item = { id: 1 };
    const list = [item, { id: 2 }];
    expect(list).toContain(item);
    expect(list).not.toContain({ id: 1 });
    expect(list).toContainEqual({ id: 1 });

    // Any iterable, not just an array.
    const set = new Set(['a', 'b']);
    expect(set).toContain('a');
    expect(set).not.toContain('c');

    expect('the quick brown fox').toContain('quick');
    expect('the quick brown fox').not.toContain('slow');
  });

  test('toContain misuse throws even under .not', async () => {
    expect(thrown(() => expect(null).toContain(1))).toContain('null nor undefined');
    expect(thrown(() => expect(null).not.toContain(1))).toContain('null nor undefined');
    expect(thrown(() => expect('hi').toContain(1))).toContain('must be a string');
    expect(thrown(() => expect(7).toContain(1))).toContain('iterable');
  });

  test('toHaveLength reads the live length', async () => {
    expect([1, 2, 3]).toHaveLength(3);
    expect(new Uint8Array(4)).toHaveLength(4);
    // A JS string's length is UTF-16 code units, not characters.
    expect('ab').toHaveLength(2);
    expect('a\u{1F600}').toHaveLength(3);
    expect(thrown(() => expect({ a: 1 }).toHaveLength(1))).toContain('length property');
  });

  test('a Page subject answers the allowed generic matchers', async ({ page }) => {
    await page.goto(dataUrl('<h1>subject</h1>'));
    expect(page).toBe(page);
    expect(page).toBeDefined();
    expect(page).toBeTruthy();
    expect(page).not.toBeNull();
    // And still answers its own web-first matchers.
    await expect(page.locator('h1')).toHaveText('subject');
  });

  test('a Locator subject answers the allowed generic matchers', async ({ page }) => {
    await page.goto(dataUrl("<button id='b'>go</button>"));
    const button = page.locator('#b');
    expect(button).toBe(button);
    expect(button).toBeTruthy();
    expect(button).not.toBe(page.locator('#b'));
    await expect(button).toBeVisible();
  });

  test('a web-first matcher on the wrong receiver names both', async () => {
    // The type says this is a mistake; the cast asks what the runtime does
    // with it, which is what a JS suite would hit.
    const wrong = expect('not a locator') as unknown as { toBeVisible(): Promise<void> };
    const msg = await (async () => {
      try {
        await wrong.toBeVisible();
        return '';
      } catch (e) {
        return String((e as Error).message ?? e);
      }
    })();
    expect(msg).toContain('toBeVisible can be only used with Locator object');
    expect(msg).toContain('not a locator');
  });

  test('expect takes a custom message', async () => {
    expect(thrown(() => expect(1, 'ids match').toBe(2))).toContain('ids match: expect(value).toBe() failed');
    expect(thrown(() => expect(1, { message: 'ids match' }).toBe(2))).toContain('ids match:');
  });

  test('expect.poll compares identity too', async () => {
    const wanted = { ready: true };
    let current: unknown = { ready: true };
    let calls = 0;
    // A structurally equal object never satisfies `toBe`; the real
    // reference, produced on the third call, does.
    await expect
      .poll(
        () => {
          calls += 1;
          if (calls >= 3) current = wanted;
          return current;
        },
        { timeout: 2000, intervals: [10] },
      )
      .toBe(wanted);
    expect(calls).toBeGreaterThanOrEqual(3);
  });
});
