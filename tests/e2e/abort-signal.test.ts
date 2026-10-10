// Ported from Playwright's page-click.spec.ts ("should abort via signal")
// and expect-timeout.spec.ts: an operation given a `signal` stops when
// it fires and fails with AbortError, the reason as its cause; an
// assertion fails as a timeout would.
import { test, expect } from '@ferridriver/test';

test.describe('signal', () => {
  test('aborts a click in flight', async ({ page }) => {
    await page.setContent(`<button style="display:none">click me</button>`);
    const controller = new AbortController();
    const promise = page.locator('button').click({ signal: controller.signal, timeout: 0 }).catch((e: Error) => e);
    await page.waitForTimeout(500);
    const reason = new Error('foo bar');
    controller.abort(reason);
    const error = await promise as Error;
    expect(error.message).toContain('foo bar');
    expect(error.message).toMatch(/Call log:[\s\S]*operation was aborted: foo bar/);
    expect(error.name).toBe('AbortError');
    expect(error.cause).toBe(reason);
  });

  test('aborts in flight with a string reason', async ({ page }) => {
    await page.setContent(`<button style="display:none">click me</button>`);
    const controller = new AbortController();
    const promise = page.locator('button').click({ signal: controller.signal, timeout: 0 });
    controller.abort('aborted by user');
    const error = await promise.catch((e: Error) => e) as Error;
    expect(error).toBeInstanceOf(Error);
    expect(error.message).toContain('aborted by user');
    expect(error.name).toBe('AbortError');
    expect(error.cause).toBe('aborted by user');
  });

  test('refuses an already-aborted signal', async ({ page }) => {
    await page.setContent(`<button>click me</button>`);
    await page.evaluate(() => { (window as any).clicked = false; document.querySelector('button')!.onclick = () => { (window as any).clicked = true; }; });
    const controller = new AbortController();
    const reason = new Error('Already aborted');
    controller.abort(reason);
    const error = await page.locator('button').click({ signal: controller.signal }).catch((e: Error) => e) as Error;
    expect(error.message).toContain('The operation was aborted');
    expect(error.name).toBe('AbortError');
    expect(error.cause).toBe(reason);
    expect(await page.evaluate(() => (window as any).clicked)).toBe(false);
  });

  test('aborts a wait and a navigation', async ({ page }) => {
    const controller = new AbortController();
    const wait = page.waitForSelector('#never', { signal: controller.signal, timeout: 0 }).catch((e: Error) => e);
    setTimeout(() => controller.abort('stop waiting'), 200);
    const error = await wait as Error;
    expect(error.name).toBe('AbortError');
    expect(error.message).toContain('stop waiting');

    // The fixture server holds this response until it is released.
    const key = `abort-signal-${Date.now()}-${Math.random()}`;
    const nav = new AbortController();
    const goto = page.goto(`http://127.0.0.1:47831/fx/control/hold/${key}`, { signal: nav.signal }).catch((e: Error) => e);
    setTimeout(() => nav.abort(new Error('navigation cancelled')), 200);
    const navError = await goto as Error;
    await page.request.get(`http://127.0.0.1:47831/fx/control/release/${key}`);
    expect(navError.name).toBe('AbortError');
    expect(navError.message).toContain('navigation cancelled');
  });

  test('a signal that never fires changes nothing', async ({ page }) => {
    await page.setContent(`<button onclick="window.clicked = true">click me</button>`);
    await page.locator('button').click({ signal: new AbortController().signal });
    expect(await page.evaluate(() => (window as any).clicked)).toBe(true);
  });

  test('fails an assertion like a timeout', async ({ page }) => {
    await page.setContent('<div>content</div>');
    const controller = new AbortController();
    const started = Date.now();
    const promise = expect(page.locator('span')).toBeVisible({ timeout: 5000, signal: controller.signal }).catch((e: Error) => e);
    await page.waitForTimeout(300);
    controller.abort(new Error('stop it'));
    const error = await promise as Error;
    expect(Date.now() - started).toBeLessThan(4000);
    expect(error.name).not.toBe('AbortError');
    expect(error.message).toContain('expect(locator).toBeVisible() failed');
  });
});
