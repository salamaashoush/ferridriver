// Ported from crates/ferridriver-cli/tests/backends_support/
// cdp_session.rs — the raw CDPSession surface. Chromium gets a live
// session (send + events + detach); Firefox/WebKit reject with the
// typed Unsupported. Test titles mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';

describe('cdp session', () => {
  test('cdp_session_page', async ({ page, context, browserName }) => {
    await page.goto('data:text/html,<body>cdp</body>');
    if (browserName !== 'chromium') {
      let msg = '';
      try {
        await context.newCDPSession(page);
      } catch (e) {
        msg = String(e);
      }
      expect(msg.includes('Chromium')).toBe(true);
      return;
    }
    const session = await context.newCDPSession(page);
    const evalResult = (await session.send('Runtime.evaluate', { expression: '6 * 7', returnByValue: true })) as {
      result: { value: number };
    };
    await session.send('Page.enable');
    const loadFired = new Promise<boolean>((resolve) => {
      session.on('Page.loadEventFired', (params) => {
        resolve(typeof (params as { timestamp?: number }).timestamp === 'number');
      });
    });
    await page.goto('data:text/html,<title>cdp-session</title>');
    const eventOk = await loadFired;

    await session.detach();
    let sendAfterDetach = '';
    try {
      await session.send('Runtime.evaluate', { expression: '1' });
    } catch (e) {
      sendAfterDetach = String(e);
    }
    let doubleDetach = '';
    try {
      await session.detach();
    } catch (e) {
      doubleDetach = String(e);
    }
    expect(evalResult.result.value).toBe(42);
    expect(eventOk).toBe(true);
    expect(sendAfterDetach.includes('detached')).toBe(true);
    expect(doubleDetach.includes('detached')).toBe(true);
  });

  test('cdp_session_browser', async ({ browser, browserName }) => {
    if (browserName !== 'chromium') {
      let msg = '';
      try {
        await browser.newBrowserCDPSession();
      } catch (e) {
        msg = String(e);
      }
      expect(msg.includes('Chromium')).toBe(true);
      return;
    }
    const session = await browser.newBrowserCDPSession();
    const version = (await session.send('Browser.getVersion')) as { product?: string };
    await session.detach();
    expect(String(version.product ?? '').includes('Chrome')).toBe(true);
  });
});
