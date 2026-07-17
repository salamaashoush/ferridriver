// Ported from crates/ferridriver-cli/tests/backends_support/
// {console_message,web_error,context_events}.rs — ConsoleMessage,
// pageerror/WebError, and the context/browser lifecycle mirror events.
// Test titles mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';
import type { BrowserContext, ConsoleMessage, Frame, Page, WebError } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

const BLANK_HTML = '<!doctype html><html><body><h1>x</h1></body></html>';

describe('events', () => {
  test('console_message_primitives', async ({ page }) => {
    // console.log('hello', 42) — two primitive args land as JSHandles
    // with the joined preview text.
    await page.goto(dataUrl(BLANK_HTML));
    const waiter = page.waitForEvent('console', { timeout: 5000 });
    await page.evaluate(() => console.log('hello', 42));
    const msg = (await waiter) as ConsoleMessage;
    expect(msg.type()).toBe('log');
    expect(msg.text().includes('hello')).toBe(true);
    expect(msg.text().includes('42')).toBe(true);
    expect(msg.args().length).toBe(2);
  });

  test('console_message_warn_maps_to_warning', async ({ page }) => {
    // console.warn surfaces as type 'warning' (Playwright parity — BiDi
    // reports method 'warn' and is remapped; CDP reports 'warning'
    // natively).
    await page.goto(dataUrl(BLANK_HTML));
    const waiter = page.waitForEvent('console', { timeout: 5000 });
    await page.evaluate(() => console.warn('careful'));
    const msg = (await waiter) as ConsoleMessage;
    expect(msg.type()).toBe('warning');
    expect(msg.text().includes('careful')).toBe(true);
  });

  test('console_message_error_type', async ({ page }) => {
    await page.goto(dataUrl(BLANK_HTML));
    const waiter = page.waitForEvent('console', { timeout: 5000 });
    await page.evaluate(() => console.error('boom'));
    const msg = (await waiter) as ConsoleMessage;
    expect(msg.type()).toBe('error');
    expect(msg.text().includes('boom')).toBe(true);
  });

  test('console_message_location_shape', async ({ page }) => {
    // location() surfaces { url, lineNumber, columnNumber } on every
    // backend. Console calls issued via evaluate don't always carry a
    // user-script URL (the devtools eval context is nameless), so the
    // check is shape-only.
    await page.goto(dataUrl(BLANK_HTML));
    const waiter = page.waitForEvent('console', { timeout: 5000 });
    await page.evaluate(() => console.log('loc-check'));
    const msg = (await waiter) as ConsoleMessage;
    const loc = msg.location();
    expect(typeof loc.url).toBe('string');
    expect(typeof loc.lineNumber).toBe('number');
    expect(typeof loc.columnNumber).toBe('number');
  });

  test('page_error_is_native_error', async ({ page }) => {
    // page.waitForEvent('pageerror') resolves to a NATIVE JS Error, not
    // a wrapper class. Polls for the specific error identifier — Firefox
    // BiDi emits a spurious cross-origin "Permission denied" error at
    // page init that would otherwise land first.
    await page.goto(dataUrl('<!doctype html><html><body><h1>wait-pageerror</h1></body></html>'));
    await page.evaluate(() => {
      setTimeout(() => {
        const e = new Error('boom');
        window.dispatchEvent(new ErrorEvent('error', { error: e, message: e.message }));
        throw e;
      }, 10);
    });
    const deadline = Date.now() + 5000;
    let match: { isError: boolean; name: string; message: string; stackIsString: boolean } | null = null;
    while (Date.now() < deadline) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      const err = (await page.waitForEvent('pageerror', { timeout: remaining })) as Error;
      if (err && err.message && err.message.includes('boom')) {
        match = {
          isError: err instanceof Error,
          name: err.name,
          message: err.message,
          stackIsString: typeof err.stack === 'string',
        };
        break;
      }
    }
    expect(match).not.toBeNull();
    expect(match!.isError).toBe(true);
    expect(match!.name).toBe('Error');
    expect(match!.message.includes('boom')).toBe(true);
    expect(match!.stackIsString).toBe(true);
  });

  test('context_weberror_is_webbed_error_class', async ({ page, context }) => {
    // context.waitForEvent('weberror') resolves to a live WebError class
    // instance with error() returning a native JS Error — exercises the
    // per-page -> per-context bridge.
    await page.goto(dataUrl('<!doctype html><html><body><h1>wait-weberror</h1></body></html>'));
    await page.evaluate(() => {
      setTimeout(() => {
        const e = new Error('ctx-forwarded');
        window.dispatchEvent(new ErrorEvent('error', { error: e, message: e.message }));
        throw e;
      }, 10);
    });
    const deadline = Date.now() + 5000;
    let match: { hasErrorMethod: boolean; isError: boolean; name: string; message: string } | null = null;
    while (Date.now() < deadline) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) break;
      const webErr = (await context.waitForEvent('weberror', { timeout: remaining })) as WebError;
      const err = webErr && typeof webErr.error === 'function' ? webErr.error() : null;
      if (err && err.message && err.message.includes('ctx-forwarded')) {
        match = {
          hasErrorMethod: typeof webErr.error === 'function',
          isError: err instanceof Error,
          name: err.name,
          message: err.message,
        };
        break;
      }
    }
    expect(match).not.toBeNull();
    expect(match!.hasErrorMethod).toBe(true);
    expect(match!.isError).toBe(true);
    expect(match!.name).toBe('Error');
    expect(match!.message.includes('ctx-forwarded')).toBe(true);
  });

  test('web_error_location', async ({ page, context }) => {
    // webError.location() (Playwright 1.60) returns { url, line, column }
    // captured from the error's top stack frame.
    await page.goto(dataUrl('<body>weberror</body>'));
    const [werr] = await Promise.all([
      context.waitForEvent('weberror', { timeout: 5000 }) as Promise<WebError>,
      page.evaluate(() => {
        setTimeout(() => {
          throw new Error('boom-loc');
        }, 10);
      }),
    ]);
    const loc = werr.location();
    expect(werr.error().name).toBe('Error');
    expect(werr.error().message).toBe('boom-loc');
    expect(typeof loc.url).toBe('string');
    expect(typeof loc.line).toBe('number');
    expect(typeof loc.column).toBe('number');
  });

  test('context_framenavigated', async ({ page, context }) => {
    // Context 'framenavigated' mirror event resolves with a Frame for
    // the navigated main frame — only fires once the page->context
    // bridge forwards the page-level frame event.
    await page.goto(dataUrl('<body>ctx-frame</body>'));
    const [frame] = await Promise.all([
      context.waitForEvent('framenavigated', { timeout: 5000 }) as Promise<Frame>,
      page.goto('data:text/html,<title>navmark</title>'),
    ]);
    expect(typeof frame.url).toBe('function');
    expect(frame.url().startsWith('data:')).toBe(true);
    expect(frame.url().includes('navmark')).toBe(true);
  });

  test('context_frameattached', async ({ page, context }) => {
    await page.goto(dataUrl('<body><div id=host></div></body>'));
    const [frame] = await Promise.all([
      context.waitForEvent('frameattached', { timeout: 5000 }) as Promise<Frame>,
      page.evaluate(() => {
        const f = document.createElement('iframe');
        f.src = 'data:text/html,<p>child</p>';
        document.getElementById('host')!.appendChild(f);
      }),
    ]);
    expect(typeof frame.url).toBe('function');
  });

  test('context_pageload', async ({ page, context }) => {
    await page.goto(dataUrl('<body>ctx-load</body>'));
    const [loaded] = await Promise.all([
      context.waitForEvent('pageload', { timeout: 5000 }) as Promise<Page>,
      page.goto('data:text/html,<title>loadmark</title>'),
    ]);
    expect(typeof loaded.url).toBe('function');
    expect(loaded.url().includes('loadmark')).toBe(true);
  });

  test('context_pageclose', async ({ page, context }) => {
    await page.goto(dataUrl('<body>ctx-close</body>'));
    const newPage = await context.newPage();
    const [closed] = await Promise.all([
      context.waitForEvent('pageclose', { timeout: 5000 }) as Promise<Page>,
      newPage.close(),
    ]);
    expect(typeof closed.isClosed).toBe('function');
    expect(closed.isClosed()).toBe(true);
  });

  test('browser_context_event', async ({ page, browser }) => {
    // browser.waitForEvent('context') fires when a new context is
    // created, resolving with the live BrowserContext.
    await page.goto(dataUrl('<body>browser-ctx-event</body>'));
    const [bcx, created] = await Promise.all([
      browser.waitForEvent('context', { timeout: 5000 }) as Promise<BrowserContext>,
      browser.newContext(),
    ]);
    try {
      expect(typeof bcx.newPage).toBe('function');
      expect(typeof bcx.cookies).toBe('function');
    } finally {
      await created.close();
    }
  });
});
