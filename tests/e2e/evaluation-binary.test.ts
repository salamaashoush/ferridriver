// Typed arrays and ArrayBuffers as evaluate arguments. Playwright sends
// them as themselves (`{ta}` / `{ab}`, isomorphic/utilityScriptSerializers.ts),
// so the page gets the same class over the same bytes, not an object of
// index keys.

import { test, expect } from '@ferridriver/test';

type Binary = {
  bytes: Uint8Array;
  floats: Float32Array;
  slice: Uint16Array;
  big: BigInt64Array;
  buffer: ArrayBuffer;
  empty: Uint8Array;
};

test('typed arrays and array buffers reach the page as themselves', async ({ page }) => {
  // A view over the middle of a larger buffer: only its own bytes cross.
  const slice = new Uint16Array(new Uint16Array([1, 2, 3, 4]).buffer, 2, 2);
  const seen = await page.evaluate(
    (arg: Binary) => ({
      bytes: [arg.bytes instanceof Uint8Array, Array.from(arg.bytes)],
      floats: [arg.floats instanceof Float32Array, Array.from(arg.floats)],
      slice: [arg.slice instanceof Uint16Array, Array.from(arg.slice)],
      big: [arg.big instanceof BigInt64Array, Array.from(arg.big, String)],
      buffer: [arg.buffer instanceof ArrayBuffer, Array.from(new Uint8Array(arg.buffer))],
      empty: [arg.empty instanceof Uint8Array, arg.empty.length],
    }),
    {
      bytes: new Uint8Array([0, 1, 254, 255]),
      floats: new Float32Array([1.5, -2.25, 3]),
      slice,
      big: new BigInt64Array([-5n, 7n]),
      buffer: new Uint8Array([9, 8, 7]).buffer,
      empty: new Uint8Array(0),
    },
  );
  expect(seen).toEqual({
    bytes: [true, [0, 1, 254, 255]],
    floats: [true, [1.5, -2.25, 3]],
    slice: [true, [2, 3]],
    big: [true, ['-5', '7']],
    buffer: [true, [9, 8, 7]],
    empty: [true, 0],
  });
});

test('a typed array survives the round trip through the page', async ({ page }) => {
  const sent = new Int32Array([-1, 0, 2147483647]);
  const back = await page.evaluate((values: Int32Array) => values, sent);
  expect(back instanceof Int32Array).toBe(true);
  expect(Array.from(back as Int32Array)).toEqual([-1, 0, 2147483647]);
});

test('a typed array crosses in a locator evaluate argument too', async ({ page }) => {
  await page.setContent('<div id="box"></div>');
  const total = await page.locator('#box').evaluate(
    (el: Element, bytes: Uint8Array) => (bytes instanceof Uint8Array ? bytes.reduce((a, b) => a + b, 0) : -1) + el.id.length,
    new Uint8Array([10, 20, 30]),
  );
  expect(total).toBe(63);
});
