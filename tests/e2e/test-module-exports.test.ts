// The framework module's shape. Playwright's `@playwright/test` is
// `Object.assign(test, exports)`: the module object IS the test
// function, the default export is that same function, and every runtime
// value hangs off it as a named export. A suite written against it uses
// all three forms, and `@playwright/test` / `playwright/test` resolve to
// the same module as `@ferridriver/test`.
//
// Identity is asserted with `===` rather than `expect(a).toBe(b)`: the
// value matchers compare a JSON snapshot of the subject, so a function
// cannot be their subject (see docs/PLAYWRIGHT-PARITY-BACKLOG.md).

import ferridriverDefault, {
  test,
  describe,
  expect,
  mergeTests,
  _baseTest,
  chromium,
  firefox,
  webkit,
} from '@ferridriver/test';
import playwrightDefault, { test as playwrightTest, expect as playwrightExpect } from '@playwright/test';
import { test as slashTest } from 'playwright/test';

describe('framework module exports', () => {
  test('the default export is the test function itself', async ({}) => {
    expect(ferridriverDefault === test).toBe(true);
    expect(typeof ferridriverDefault).toBe('function');
    expect(typeof (ferridriverDefault as unknown as { extend: unknown }).extend).toBe('function');
  });

  test('the playwright specifiers resolve to the same module', async ({}) => {
    expect(playwrightTest === test).toBe(true);
    expect(slashTest === test).toBe(true);
    expect(playwrightDefault === test).toBe(true);
    expect(playwrightExpect === expect).toBe(true);
  });

  test('the runtime values are named exports', async ({}) => {
    for (const [name, value] of Object.entries({ test, describe, expect, mergeTests, _baseTest })) {
      expect(`${name}:${typeof value}`).toBe(`${name}:function`);
    }
    for (const [name, value] of Object.entries({ chromium, firefox, webkit })) {
      expect(`${name}:${typeof value}`).not.toBe(`${name}:undefined`);
    }
  });

  test('_baseTest is the unextended root, not a copy', async ({}) => {
    const extended = test.extend({ marker: async ({}, use: (v: number) => Promise<void>) => use(1) });
    expect(_baseTest === test).toBe(true);
    expect(extended === test).toBe(false);
  });

  test('require() hands back the same module object', async ({}) => {
    const required = (globalThis as unknown as { require: (s: string) => Record<string, unknown> }).require(
      '@playwright/test',
    );
    expect(required.test === test).toBe(true);
    expect(required.expect === expect).toBe(true);
    expect(typeof required).toBe('function');
  });
});
