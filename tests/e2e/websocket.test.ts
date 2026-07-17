// Ported from crates/ferridriver-cli/tests/backends_support/
// {network,route_web_socket}.rs (WebSocket half) — live WebSocket frame
// observation via page.waitForEvent('websocket') against the fixture
// server's /fx/ws echo, and the fully-mocked routeWebSocket path
// (page/context scope, iframe realm anchoring, future pages, scope
// precedence). Test titles mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';
import type { WebSocket as WsInfo } from '@ferridriver/test';
import { fxWsUrl } from './helpers/server';

describe('websocket', () => {
  test('network_websocket', async ({ page, baseURL, browserName }) => {
    if (browserName === 'firefox') {
      // W3C BiDi exposes no WebSocket frame events (Playwright's own
      // BiDi backend skips WebSocket handling for the same reason) —
      // the wait rejects with a typed Timeout rather than dangling.
      await page.goto('about:blank');
      let msg = '';
      try {
        await page.waitForEvent('websocket', { timeout: 500 });
      } catch (e) {
        msg = String((e as Error).message ?? e).toLowerCase();
      }
      expect(msg.includes('timeout') || msg.includes('waiting for event')).toBe(true);
      return;
    }
    // CDP exposes Network.webSocketFrameSent/Received; the WebKit
    // Inspector protocol mirrors the same event family. A real http
    // origin keeps Chromium's Private Network Access from blocking the
    // loopback WebSocket.
    await page.goto('/fx/landed');
    const wsUrl = fxWsUrl(baseURL);
    const wsPromise = page.waitForEvent('websocket', { timeout: 10000 });
    await page.evaluate(
      `window.__ws = new WebSocket(${JSON.stringify(wsUrl)});` +
        'window.__opened = new Promise((res) => { window.__ws.onopen = () => res(); }); null',
    );
    const ws = (await wsPromise) as WsInfo;
    const recvPromise = ws.waitForEvent('framereceived', { timeout: 10000 });
    await page.evaluate("window.__opened.then(() => window.__ws.send('hello-ws')); null");
    const frame = await recvPromise;
    expect(ws.url()).toBe(wsUrl);
    expect(frame.payload).toBe('hello-ws');
  });

  test('page_route_web_socket', async ({ page }) => {
    // Fully-mocked path: the handler sets onMessage to echo back a
    // prefixed reply; the page never reaches a real server. The reply is
    // observed with the idiomatic Playwright single-await shape — one
    // page.evaluate returns a page-side promise resolved by the
    // driver->page WS dispatch while the script is parked on that await.
    await page.routeWebSocket('ws://ferri.invalid/mock', (ws) => {
      ws.onMessage((m) => ws.send('mocked:' + m));
    });
    await page.goto('/fx/landed');
    const got = await page.evaluate(
      (u: string) =>
        new Promise((resolve) => {
          const ws = new WebSocket(u);
          ws.onopen = () => ws.send('hi');
          ws.onmessage = (e) => resolve(e.data);
        }),
      'ws://ferri.invalid/mock',
    );
    expect(got).toBe('mocked:hi');
  });

  test('context_route_web_socket', async ({ page, context }) => {
    // Same echo handler registered at the context level — the
    // context-level fan-out reaches the same WS mock + pump.
    await context.routeWebSocket('ws://ferri.invalid/ctxmock', (ws) => {
      ws.onMessage((m) => ws.send('ctx:' + m));
    });
    await page.goto('/fx/landed');
    const got = await page.evaluate(
      (u: string) =>
        new Promise((resolve) => {
          const ws = new WebSocket(u);
          ws.onopen = () => ws.send('hi');
          ws.onmessage = (e) => resolve(e.data);
        }),
      'ws://ferri.invalid/ctxmock',
    );
    expect(got).toBe('ctx:hi');
  });

  test('page_route_web_socket_in_iframe', async ({ page }) => {
    // A socket created INSIDE a same-origin iframe: the onCreate binding
    // call carries the iframe as its BindingSource.frame, and every
    // driver->page dispatch (ensureOpened, send) must evaluate in THAT
    // frame — the iframe realm has its own WebSocket mock and
    // idToWebSocket map, so a main-frame dispatch silently strands the
    // socket. Mirrors Playwright's source.frame.evaluateExpression
    // anchoring in webSocketRouteDispatcher.ts.
    await page.routeWebSocket('ws://ferri.invalid/frame-mock', (ws) => {
      ws.onMessage((m) => ws.send('frame:' + m));
    });
    await page.goto('/fx/iframe');
    await page.waitForSelector('iframe');
    let frame = null;
    for (let i = 0; i < 50; i++) {
      const frames = page.frames();
      if (frames.length > 1) {
        frame = frames[1];
        break;
      }
      await page.waitForTimeout(100);
    }
    expect(frame).not.toBeNull();
    const got = await frame!.evaluate(
      (u: string) =>
        new Promise((resolve) => {
          const ws = new WebSocket(u);
          ws.onopen = () => ws.send('hi');
          ws.onmessage = (e) => resolve(e.data);
        }),
      'ws://ferri.invalid/frame-mock',
    );
    expect(got).toBe('frame:hi');
  });

  test('context_route_web_socket_future_page', async ({ browser, baseURL }) => {
    // context.routeWebSocket applies to pages opened AFTER the route was
    // registered — context-scoped interception patterns, not a snapshot
    // fan-out.
    const ctx = await browser.newContext();
    try {
      await ctx.routeWebSocket('ws://ferri.invalid/future', (ws) => {
        ws.onMessage((m) => ws.send('future:' + m));
      });
      const p = await ctx.newPage();
      await p.goto(`${baseURL}/fx/landed`);
      const got = await p.evaluate(
        (u: string) =>
          new Promise((resolve) => {
            const ws = new WebSocket(u);
            ws.onopen = () => ws.send('hi');
            ws.onmessage = (e) => resolve(e.data);
          }),
        'ws://ferri.invalid/future',
      );
      expect(got).toBe('future:hi');
    } finally {
      await ctx.close();
    }
  });

  test('route_web_socket_scope_precedence', async ({ page, context }) => {
    // Page-scope routes beat context-scope routes for the same URL, and
    // within page scope the newest registration wins — Playwright's
    // page._onWebSocketRoute falls through to the context handler list,
    // each searched newest-first.
    const wsUrl = 'ws://ferri.invalid/prec';
    await context.routeWebSocket(wsUrl, (ws) => {
      ws.onMessage((m) => ws.send('ctx:' + m));
    });
    await page.routeWebSocket(wsUrl, (ws) => {
      ws.onMessage((m) => ws.send('page-old:' + m));
    });
    await page.routeWebSocket(wsUrl, (ws) => {
      ws.onMessage((m) => ws.send('page-new:' + m));
    });
    await page.goto('/fx/landed');
    const got = await page.evaluate(
      (u: string) =>
        new Promise((resolve) => {
          const ws = new WebSocket(u);
          ws.onopen = () => ws.send('hi');
          ws.onmessage = (e) => resolve(e.data);
        }),
      wsUrl,
    );
    expect(got).toBe('page-new:hi');
  });
});
