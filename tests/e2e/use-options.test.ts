// The runner options Playwright spells inside `use`. `actionTimeout`,
// `navigationTimeout` and `baseURL` are test-scoped there, so a spec may
// set them; `trace`, `video` and `screenshot` are worker options and are
// config/project-level here for the same reason.

import { test, describe, expect } from '@ferridriver/test';

describe('use: actionTimeout', () => {
  test.use({ actionTimeout: 400 });

  test('an action gives up after actionTimeout, not the default', async ({ page }) => {
    await page.setContent('<body><p>no button here</p></body>');
    const started = Date.now();
    let message = '';
    try {
      await page.click('#missing');
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    const elapsed = Date.now() - started;

    expect(message.length > 0).toBe(true);
    // The default is 30s; anything under a couple of seconds can only be
    // the configured 400ms.
    expect(elapsed < 5000).toBe(true);
  });
});

describe('use: navigationTimeout', () => {
  test.use({ navigationTimeout: 500 });

  test('a navigation gives up after navigationTimeout', async ({ page, baseURL }) => {
    const started = Date.now();
    let message = '';
    try {
      // The fixture server holds this route open past the timeout.
      await page.goto(`${baseURL}/fx/slow?ms=8000`);
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    const elapsed = Date.now() - started;

    expect(message.length > 0).toBe(true);
    expect(elapsed < 5000).toBe(true);
  });
});

describe('use: baseURL', () => {
  test('a relative goto resolves against the configured baseURL', async ({ page, baseURL }) => {
    const response = await page.goto('/fx/landed');
    expect(response !== null).toBe(true);
    expect(page.url().startsWith(baseURL!)).toBe(true);
  });
});
