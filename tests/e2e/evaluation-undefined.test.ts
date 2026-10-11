// `undefined` inside an evaluate argument. Playwright's `serializeValue`
// (packages/protocol/src/serializers.ts) walks `Object.keys` and every
// array index, so an undefined property keeps its key, a hole or an
// undefined item stays `undefined` rather than `null`, and a symbol value
// crosses as `undefined`.

import { test, expect } from '@ferridriver/test';

type Sparse = { gone?: number; symbol: unknown; list: unknown[]; nested: { inner?: string } };

test('undefined properties and items keep their place', async ({ page }) => {
  const seen = await page.evaluate(
    (arg: Sparse) => ({
      keys: Object.keys(arg).sort(),
      gone: 'gone' in arg && arg.gone === undefined,
      symbol: 'symbol' in arg && arg.symbol === undefined,
      list: arg.list.map(item => (item === undefined ? 'undefined' : item === null ? 'null' : item)),
      nested: 'inner' in arg.nested && arg.nested.inner === undefined,
    }),
    { gone: undefined, symbol: Symbol('s'), list: [1, undefined, null, , 5], nested: { inner: undefined } },
  );
  expect(seen).toEqual({
    keys: ['gone', 'list', 'nested', 'symbol'],
    gone: true,
    symbol: true,
    list: [1, 'undefined', 'null', 'undefined', 5],
    nested: true,
  });
});

test('an exposed function returns undefined properties', async ({ page }) => {
  await page.exposeFunction('sparse', () => ({ gone: undefined, list: [undefined] }));
  const seen = await page.evaluate(async () => {
    const value = await (window as any).sparse();
    return { gone: 'gone' in value && value.gone === undefined, item: value.list.length === 1 && value.list[0] === undefined };
  });
  expect(seen).toEqual({ gone: true, item: true });
});
