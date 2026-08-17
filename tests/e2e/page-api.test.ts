// Ported from crates/ferridriver-cli/tests/backends_support/page_api.rs —
// the page event emitter (on/once/off/removeAllListeners), retained
// console/pageerror history, waitForTimeout, addScriptTag/addStyleTag,
// setExtraHTTPHeaders, isEditable, viewportSize, the context()
// accessor, $eval/$$eval, pause, requestGC, locator.describe, and the
// TimeoutError shape. Test titles mirror the original Rust fn names.
//
// Event listeners fire cross-task (a backend tokio task re-enters the
// script VM), so the event tests yield via real page round-trips
// (`page.title()`) to let the dispatch task deliver — a synchronous
// busy-loop would starve it.

import { test, describe, expect } from '@ferridriver/test';
import type { ConsoleMessage, Page } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

const H1 = dataUrl('<h1>page-api</h1>');

async function pollUntil(page: Page, pred: () => boolean, timeoutMs = 4000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!pred() && Date.now() < deadline) {
    await page.title();
  }
}

// Give any (incorrect) late delivery a chance to land before asserting
// that nothing more arrived.
async function settle(page: Page): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await page.title();
  }
}

describe('page api', () => {
  test('script_eval_on_selector', async ({ page }) => {
    // Playwright: `page.$eval(sel, fn, arg?)` runs the function on the
    // FIRST match; `page.$$eval(sel, fn, arg?)` on ALL matches as an
    // array. Each assertion observes a DOM-derived value that only
    // appears when the function actually ran against the resolved
    // element(s).
    await page.goto(dataUrl("<ul><li data-v='a'>one</li><li data-v='b'>two</li></ul>"));
    expect(await page.$eval('li', (el: Element) => el.getAttribute('data-v'))).toBe('a');
    expect(await page.$eval('li', (el: Element, s: string) => el.textContent + s, '!')).toBe('one!');
    expect(await page.$$eval('li', (els: Element[]) => els.map((e) => e.textContent))).toEqual(['one', 'two']);

    // $eval rejects when the selector matches nothing (Playwright's
    // evalOnSelector).
    let missThrew = false;
    try {
      await page.$eval('.nope', (el: Element) => el.tagName);
    } catch {
      missThrew = true;
    }
    expect(missThrew).toBe(true);
  });

  test('script_page_pause_unsupported', async ({ page }) => {
    // ferridriver has no Inspector UI, so pause() rejects with a typed
    // Unsupported error rather than a silent no-op.
    await page.goto(H1);
    let msg = '';
    try {
      await page.pause();
    } catch (e) {
      msg = String((e as Error).message ?? e).toLowerCase();
    }
    expect(msg).toMatch(/pause|inspector|unsupported/);
  });

  test('page_on_receives_console', async ({ page }) => {
    // `page.on('console', cb)` delivers a live ConsoleMessage instance
    // (`type()` / `text()` methods — the same object
    // `waitForEvent('console')` resolves to).
    await page.goto(H1);
    const got: Array<{ type: string; text: string }> = [];
    page.on('console', (msg) => {
      const m = msg as ConsoleMessage;
      got.push({ type: m.type(), text: m.text() });
    });
    await page.evaluate("console.log('on-hello', 7)");
    await pollUntil(page, () => got.length > 0);
    expect(got.length).toBeGreaterThanOrEqual(1);
    expect(got[0].type).toBe('log');
    expect(got[0].text).toContain('on-hello');
  });

  test('page_off_stops_delivery', async ({ page }) => {
    // `page.off(event, listener)` removes the registration by function
    // identity: the event after `off` is not delivered, while the one
    // before it was.
    await page.goto(H1);
    const got: string[] = [];
    const listener = (msg: unknown) => got.push((msg as ConsoleMessage).text());
    expect(page.on('console', listener) === page).toBe(true);
    await page.evaluate("console.log('before-off')");
    await pollUntil(page, () => got.length > 0);
    const afterFirst = got.length;
    expect(afterFirst).toBeGreaterThanOrEqual(1);
    page.off('console', listener);
    await page.evaluate("console.log('after-off')");
    await settle(page);
    expect(got.length).toBe(afterFirst);
  });

  test('page_once_fires_once', async ({ page }) => {
    // `page.once(event, cb)` fires at most once even when the event
    // recurs (core auto-removes after the first emit).
    await page.goto(H1);
    const got: string[] = [];
    page.once('console', (msg) => got.push((msg as ConsoleMessage).text()));
    await page.evaluate("console.log('once-1')");
    await pollUntil(page, () => got.length > 0);
    await page.evaluate("console.log('once-2')");
    await settle(page);
    expect(got.length).toBe(1);
  });

  test('page_remove_all_listeners', async ({ page }) => {
    await page.goto(H1);
    const got: string[] = [];
    page.on('console', (msg) => got.push((msg as ConsoleMessage).text()));
    await page.evaluate("console.log('pre-clear')");
    await pollUntil(page, () => got.length > 0);
    const before = got.length;
    expect(before).toBeGreaterThanOrEqual(1);
    page.removeAllListeners();
    await page.evaluate("console.log('post-clear')");
    await settle(page);
    expect(got.length).toBe(before);
  });

  test('page_on_pageerror_is_error', async ({ page }) => {
    // `page.on('pageerror', cb)` hands the listener a native JS Error.
    await page.goto(H1);
    const got: Array<{ isError: boolean; message: string; name: string }> = [];
    page.on('pageerror', (err) => {
      const e = err as Error;
      got.push({ isError: e instanceof Error, message: e.message, name: e.name });
    });
    await page.evaluate("setTimeout(() => { throw new Error('listener-boom'); }, 5)");
    await pollUntil(page, () => got.some((e) => (e.message || '').includes('listener-boom')), 5000);
    const hit = got.find((e) => (e.message || '').includes('listener-boom'));
    expect(hit).toBeDefined();
    expect(hit?.isError).toBe(true);
    expect(hit?.name).toBe('Error');
  });

  test('page_wait_for_timeout', async ({ page }) => {
    await page.goto(H1);
    const t0 = Date.now();
    await page.waitForTimeout(150);
    expect(Date.now() - t0).toBeGreaterThanOrEqual(120);
  });

  test('page_bring_to_front', async ({ page }) => {
    // `page.bringToFront()` activates the page —
    // `document.visibilityState` is 'visible' afterwards.
    await page.goto(H1);
    await page.bringToFront();
    expect(await page.evaluate('document.visibilityState')).toBe('visible');
  });

  test('page_add_script_tag', async ({ page }) => {
    await page.goto(H1);
    await page.addScriptTag({ content: "window.__addedByTag = 'script-ok';" });
    expect(await page.evaluate('window.__addedByTag')).toBe('script-ok');
  });

  test('page_add_style_tag', async ({ page }) => {
    await page.goto(H1);
    await page.addStyleTag({ content: 'h1 { color: rgb(1, 2, 3); }' });
    expect(await page.evaluate("getComputedStyle(document.querySelector('h1')).color")).toBe('rgb(1, 2, 3)');
  });

  test('page_is_editable', async ({ page }) => {
    await page.goto(dataUrl('<input id=a><input id=b disabled>'));
    expect(await page.isEditable('#a')).toBe(true);
    expect(await page.isEditable('#b')).toBe(false);
  });

  test('page_viewport_size', async ({ page }) => {
    await page.goto(H1);
    await page.setViewportSize({ width: 820, height: 610 });
    const vs = page.viewportSize();
    expect(vs?.width).toBe(820);
    expect(vs?.height).toBe(610);
  });

  test('page_context_accessor', async ({ page }) => {
    // `page.context()` returns the owning BrowserContext — a real
    // binding with the context surface on it.
    await page.goto(H1);
    const ctx = page.context();
    expect(ctx == null).toBe(false);
    expect(typeof ctx.newPage).toBe('function');
  });

  test('page_set_extra_http_headers', async ({ page }) => {
    // The extra header rides on every request the page initiates —
    // observed via the fixture server's /fx/echo-headers JSON echo,
    // fetched from inside the page (the header applies to subresource
    // requests exactly like document requests).
    await page.setExtraHTTPHeaders({ 'x-page-extra': 'present' });
    await page.goto('/fx/landed');
    const seen = await page.evaluate(
      "fetch('/fx/echo-headers').then(r => r.json()).then(j => j['x-page-extra'] || '')",
    );
    expect(seen).toBe('present');
  });

  test('page_off_by_function', async ({ page }) => {
    // `page.off(event, listener)` removes the registration matching the
    // given function by identity (Playwright's `off` shape) while a
    // second listener for the same event keeps firing.
    await page.goto(H1);
    const a: string[] = [];
    const b: string[] = [];
    const la = (msg: unknown) => a.push((msg as ConsoleMessage).text());
    const lb = (msg: unknown) => b.push((msg as ConsoleMessage).text());
    page.on('console', la);
    page.on('console', lb);
    await page.evaluate("console.log('both')");
    await pollUntil(page, () => a.length > 0 && b.length > 0);
    page.off('console', la);
    await page.evaluate("console.log('only-b')");
    await pollUntil(page, () => b.some((t) => t.includes('only-b')));
    page.off('console', lb);
    expect(a.some((t) => t.includes('both'))).toBe(true);
    expect(a.some((t) => t.includes('only-b'))).toBe(false);
    expect(b.some((t) => t.includes('both'))).toBe(true);
    expect(b.some((t) => t.includes('only-b'))).toBe(true);
  });

  test('wait_for_event_predicate', async ({ page }) => {
    // `page.waitForEvent(event, { predicate })` skips non-matching
    // events and resolves with the first live object the predicate
    // accepts (Playwright's optionsOrPredicate shape).
    await page.goto(H1);
    const waiter = page.waitForEvent('console', {
      predicate: (msg) => (msg as ConsoleMessage).text().includes('pick-me'),
      timeout: 8000,
    });
    await page.evaluate("console.log('skip-1'); console.log('pick-me');");
    const msg = (await waiter) as ConsoleMessage;
    expect(msg.text()).toContain('pick-me');
    expect(msg.type()).toBe('log');
  });

  test('page_console_messages', async ({ page }) => {
    // `page.consoleMessages()` returns the retained history: the
    // default filter only spans messages after the last main-frame
    // navigation, `{ filter: 'all' }` spans page lifetime, and
    // `page.clearConsoleMessages()` drops everything.
    await page.goto(H1);
    await page.evaluate("console.log('before-nav-msg')");
    // Reload starts a new since-navigation window.
    await page.reload();
    await page.evaluate("console.log('after-nav-msg')");
    let since: ConsoleMessage[] = [];
    await pollUntil(
      page,
      () => {
        since = page.consoleMessages();
        return since.some((m) => m.text().includes('after-nav-msg'));
      },
    );
    const all = page.consoleMessages({ filter: 'all' });
    const sinceTexts = since.map((m) => m.text());
    const allTexts = all.map((m) => m.text());
    expect(sinceTexts.some((t) => t.includes('after-nav-msg'))).toBe(true);
    expect(sinceTexts.some((t) => t.includes('before-nav-msg'))).toBe(false);
    expect(allTexts.some((t) => t.includes('before-nav-msg'))).toBe(true);
    expect(allTexts.some((t) => t.includes('after-nav-msg'))).toBe(true);
    page.clearConsoleMessages();
    expect(page.consoleMessages({ filter: 'all' }).length).toBe(0);
  });

  test('page_page_errors', async ({ page }) => {
    // `page.pageErrors()` returns retained uncaught exceptions as
    // native Errors; `page.clearPageErrors()` drops them.
    await page.goto(H1);
    await page.evaluate("setTimeout(() => { throw new Error('retained-boom'); }, 5)");
    let errs: Error[] = [];
    await pollUntil(
      page,
      () => {
        errs = page.pageErrors();
        return errs.some((e) => (e.message || '').includes('retained-boom'));
      },
      5000,
    );
    const hit = errs.find((e) => (e.message || '').includes('retained-boom'));
    expect(hit).toBeDefined();
    expect(hit instanceof Error).toBe(true);
    expect(hit?.name).toBe('Error');
    page.clearPageErrors();
    expect(page.pageErrors({ filter: 'all' }).length).toBe(0);
  });

  test('page_request_gc', async ({ page, browserName }) => {
    // `page.requestGC()` collects unreachable objects — observed via a
    // WeakRef whose referent was dropped (Playwright's own
    // page-request-gc.spec.ts pattern). On Firefox the call needs a
    // TestUtils.gc()-exposing build; absent that it must surface the
    // typed Unsupported error rather than silently succeeding.
    await page.goto(H1);
    await page.evaluate(
      "globalThis.objectToDestroy = { hello: 'world' }; globalThis.weakRef = new WeakRef(globalThis.objectToDestroy);",
    );
    try {
      await page.requestGC();
    } catch (e) {
      expect(browserName).toBe('firefox');
      expect(String((e as Error).message ?? e)).toContain('requestGC');
      return;
    }
    // Reachable object must survive GC.
    expect(await page.evaluate("globalThis.weakRef.deref() ? 'live' : 'collected'")).toBe('live');
    await page.evaluate('globalThis.objectToDestroy = null');
    let after = 'live';
    for (let i = 0; i < 10 && after === 'live'; i++) {
      await page.requestGC();
      after = (await page.evaluate("globalThis.weakRef.deref() ? 'live' : 'collected'")) as string;
    }
    expect(after).toBe('collected');
  });

  test('locator_describe', async ({ page }) => {
    // `locator.describe(description)` decorates the selector without
    // affecting matching — the described locator still resolves and
    // acts.
    await page.goto(dataUrl("<button id=go onclick='window.__describedClick = 1'>Go</button>"));
    const described = page.locator('#go').describe('the go button');
    expect(await described.count()).toBe(1);
    await described.click();
    expect(await page.evaluate('window.__describedClick === 1')).toBe(true);
  });

  test('timeout_error_name', async ({ page }) => {
    // Timeouts surface as a real JS Error with name === 'TimeoutError'
    // and the core message (Playwright shape).
    await page.goto(H1);
    let caught: Error | null = null;
    try {
      await page.waitForSelector('#does-not-exist', { timeout: 250 });
    } catch (e) {
      caught = e as Error;
    }
    expect(caught).not.toBeNull();
    expect(caught instanceof Error).toBe(true);
    expect(caught?.name).toBe('TimeoutError');
    expect(String(caught?.message).startsWith('Timeout 250ms exceeded')).toBe(true);
  });
});
