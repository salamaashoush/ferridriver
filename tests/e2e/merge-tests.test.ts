// `mergeTests` composes independent `test.extend` chains. What makes it
// more than concatenation is the common-ancestor rule: a fixture the two
// chains SHARE must be registered once, or the shared registration
// becomes an override of itself and resolves its own `super`.
//
// The observable is the page itself: `shared` wraps the built-in `page`
// and stamps a mark on it, so a merged chain that holds the wrapper
// twice sets the page up twice and the mark is doubled.

import { test as base, describe, expect, mergeTests } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

const shared = base.extend<{ page: Page }>({
  page: async ({ page }: { page: Page }, use: (p: Page) => Promise<void>) => {
    await page.evaluate("globalThis.__seed = (globalThis.__seed ?? '') + 'x'");
    await use(page);
  },
});

const withAlpha = shared.extend<{ alpha: string }>({
  alpha: async ({}, use: (v: string) => Promise<void>) => use('a'),
});

const withBeta = shared.extend<{ beta: string }>({
  beta: async ({}, use: (v: string) => Promise<void>) => use('b'),
});

const merged = mergeTests(withAlpha, withBeta);

describe('mergeTests', () => {
  merged(
    'resolves fixtures from both chains and sets the shared ancestor up once',
    async ({ page, alpha, beta }: { page: Page; alpha: string; beta: string }) => {
      expect(alpha).toBe('a');
      expect(beta).toBe('b');
      expect(await page.evaluate('globalThis.__seed')).toBe('x');
    },
  );

  withAlpha('the un-merged chain sets it up once too', async ({ page }: { page: Page }) => {
    expect(await page.evaluate('globalThis.__seed')).toBe('x');
  });

  merged('a merged test extends further', async ({}) => {
    const further = merged.extend<{ gamma: string }>({
      gamma: async ({}, use: (v: string) => Promise<void>) => use('c'),
    });
    expect(typeof further.extend).toBe('function');
  });

  base('mergeTests rejects a non-test argument with Playwright\'s message', async ({}) => {
    let message = '';
    try {
      (mergeTests as unknown as (...args: unknown[]) => unknown)({});
    } catch (e) {
      message = String((e as Error).message);
    }
    expect(message).toContain('mergeTests() accepts "test" functions as parameters.');
    expect(message).toContain('Did you mean to call test.extend() with fixtures instead?');
  });

  base('test.extend rejects a test object with Playwright\'s message', async ({}) => {
    let message = '';
    try {
      (base.extend as unknown as (arg: unknown) => unknown)(withAlpha);
    } catch (e) {
      message = String((e as Error).message);
    }
    expect(message).toContain('test.extend() accepts fixtures object, not a test object.');
    expect(message).toContain('Did you mean to call mergeTests()?');
  });
});
