// Structural equality over the live values: `Map`, `Set`, `Date`,
// `RegExp`, `Error`, typed arrays, `bigint`, class identity,
// `undefined`-valued keys, array holes and cycles all mean what they
// mean in Playwright. Runs on every backend project — the engine is
// shared, and a value read back out of a page has to compare the same
// way on each of them.

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

describe('expect equality', () => {
  test('maps and sets compare by content', async () => {
    expect(new Map([['a', 1]])).toEqual(new Map([['a', 1]]));
    expect(new Map([['a', 1]])).not.toEqual(new Map([['a', 2]]));
    expect(new Map([['a', 1]])).not.toEqual(new Map());
    // Order-insensitive, as jest's iterableEquality is.
    expect(new Set([1, 2])).toEqual(new Set([2, 1]));
    expect(new Set([1, 2])).not.toEqual(new Set([1, 3]));
    // A Map is not the plain object with the same entries.
    expect(new Map([['a', 1]])).not.toEqual({ a: 1 });
    // Nested, with an asymmetric matcher inside.
    expect({ m: new Map([['a', { b: 1 }]]) }).toEqual({ m: new Map([['a', { b: expect.any(Number) }]]) });
  });

  test('dates, regexps and errors compare by value', async () => {
    expect(new Date(5)).toEqual(new Date(5));
    expect(new Date(5)).not.toEqual(new Date(6));
    expect(new Date(NaN)).toEqual(new Date(NaN));
    expect(/ab+/gi).toEqual(/ab+/gi);
    expect(/ab+/g).not.toEqual(/ab+/i);
    expect(new Error('boom')).toEqual(new Error('boom'));
    expect(new RangeError('x')).not.toEqual(new Error('x'));
  });

  test('toEqual ignores undefined keys, toStrictEqual does not', async () => {
    expect({ a: 1, b: undefined }).toEqual({ a: 1 });
    expect({ a: 1 }).toEqual({ a: 1, b: undefined });
    expect({ a: 1, b: undefined }).not.toStrictEqual({ a: 1 });
    expect({ a: 1, b: undefined }).toStrictEqual({ a: 1, b: undefined });
  });

  test('toStrictEqual compares the class and array holes', async () => {
    class Point {
      x = 1;
    }
    expect(new Point()).toEqual({ x: 1 });
    expect(new Point()).not.toStrictEqual({ x: 1 });
    expect(new Point()).toStrictEqual(new Point());
    expect([, 1]).toEqual([undefined, 1]);
    expect([, 1]).not.toStrictEqual([undefined, 1]);
    expect([, 1]).toStrictEqual([, 1]);
  });

  test('bigints and typed arrays compare', async () => {
    expect(1n).toEqual(1n);
    expect(1n).not.toEqual(2n);
    expect(new Uint8Array([1, 2])).toEqual(new Uint8Array([1, 2]));
    expect(new Uint8Array([1, 2])).not.toEqual(new Uint8Array([1, 3]));
  });

  test('a cyclic structure terminates', async () => {
    const a: Record<string, unknown> = { name: 'a' };
    a.self = a;
    const b: Record<string, unknown> = { name: 'a' };
    b.self = b;
    expect(a).toEqual(b);
    const c: Record<string, unknown> = { name: 'c' };
    c.self = c;
    expect(a).not.toEqual(c);
  });

  test('toHaveProperty reads the property, getters included', async () => {
    class Holder {
      get computed() {
        return 7;
      }
    }
    expect(new Holder()).toHaveProperty('computed', 7);
    expect({ a: { b: 42 } }).toHaveProperty('a.b', 42);
    expect({ arr: [10, 20] }).toHaveProperty(['arr', 1], 20);
    expect({ m: new Date(3) }).toHaveProperty('m', new Date(3));
    expect({ a: 1 }).not.toHaveProperty('b');
  });

  test('toContainEqual is deep over live items', async () => {
    expect([{ a: 1 }]).toContainEqual({ a: 1 });
    expect([new Date(1)]).toContainEqual(new Date(1));
    expect(new Set([{ a: 1 }])).toContainEqual({ a: 1 });
    expect([{ a: 1 }]).not.toContainEqual({ a: 2 });
  });

  test('a failure still prints expected and received', async () => {
    const msg = thrown(() => expect({ a: 1 }).toEqual({ a: 2 }));
    expect(msg).toContain('toEqual');
    expect(msg).toContain('Expected');
    expect(msg).toContain('Received');
  });

  test('values read out of a page compare the same way', async ({ page }) => {
    await page.goto(dataUrl("<div id='d' data-n='2'>x</div>"));
    const attrs = await page.evaluate("JSON.parse(JSON.stringify({ n: document.getElementById('d').dataset.n }))");
    expect(attrs).toEqual({ n: '2' });
    expect(attrs).toMatchObject({ n: expect.any(String) });
    const count = await page.locator('#d').count();
    expect(count).toEqual(1);
  });
});
