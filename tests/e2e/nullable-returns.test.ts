// Every accessor Playwright declares as `T | null` answers `null`, not
// `undefined` — a Rust `Option::None` lowers to `undefined` unless the
// binding says otherwise, and `=== null` / `toBeNull()` are what users
// write against these (client/network.ts, client/frame.ts,
// client/elementHandle.ts).

import { test, describe, expect } from '@ferridriver/test';

describe('nullable returns', () => {
  test('page_and_frame_accessors_answer_null', async ({ page }) => {
    await page.goto('/fx/landed');

    expect(await page.$('#definitely-not-here')).toBeNull();
    expect(page.frame('no-such-frame')).toBeNull();
    expect(page.mainFrame().parentFrame()).toBeNull();
  });

  test('element_accessors_answer_null', async ({ page }) => {
    await page.goto('/fx/landed');
    const body = await page.$('body');
    expect(body).not.toBeNull();

    expect(await body!.getAttribute('data-not-set')).toBeNull();
    expect(await body!.contentFrame()).toBeNull();
    expect(await page.locator('body').getAttribute('data-not-set')).toBeNull();
    expect(page.locator('body').description()).toBeNull();
  });

  test('request_and_response_accessors_answer_null', async ({ page }) => {
    await page.goto('/fx/landed');
    const wait = page.waitForRequest('**/fx/landed', { timeout: 10000 });
    const [req] = await Promise.all([wait, page.reload()]);

    // A GET carries no body and was not redirected to.
    expect(req.postData()).toBeNull();
    expect(req.postDataBuffer()).toBeNull();
    expect(req.redirectedFrom()).toBeNull();
    expect(await req.headerValue('x-not-sent')).toBeNull();

    const resp = await req.response();
    expect(resp).not.toBeNull();
    expect(await resp!.headerValue('x-not-sent')).toBeNull();
  });

  test('web_storage_answers_null_for_a_missing_key', async ({ page }) => {
    await page.goto('/fx/landed');
    expect(await page.localStorage.getItem('never-written')).toBeNull();
    expect(await page.sessionStorage.getItem('never-written')).toBeNull();
  });
});
