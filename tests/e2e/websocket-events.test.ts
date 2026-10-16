// webSocket.on / once / off and waitForEvent with Playwright's shapes:
// { payload } for frames, the socket for 'close'. Ported from
// Playwright's web-socket.spec.ts against the fixture /fx/ws echo.
import { test, describe, expect } from '@ferridriver/test';
import type { Page, WebSocket, WebSocketFrame } from '@ferridriver/test';
import { fxWsUrl } from './helpers/server';

async function openSocket(page: Page, baseURL: string | undefined): Promise<WebSocket> {
  await page.goto('/fx/landed');
  const socket = page.waitForEvent('websocket');
  await page.evaluate((url: string) => {
    const ws = new WebSocket(url);
    ws.binaryType = 'arraybuffer';
    (window as any).__ws = ws;
    (window as any).__opened = new Promise(resolve => { ws.onopen = resolve; });
  }, fxWsUrl(baseURL));
  await page.evaluate(() => (window as any).__opened);
  return await socket as WebSocket;
}

describe('websocket events', () => {
  test.beforeEach(({ browserName }) => {
    // WebDriver BiDi reports no WebSocket traffic; Playwright's BiDi
    // backend has no WebSocket events either.
    test.skip(browserName === 'firefox', 'WebDriver BiDi has no WebSocket events');
  });

  test('listeners hear sent and received frames until removed', async ({ page, baseURL }) => {
    const ws = await openSocket(page, baseURL);
    const log: string[] = [];
    const sent = (frame: WebSocketFrame) => log.push(`sent:${frame.payload}`);
    ws.on('framesent', sent);
    ws.on('framereceived', frame => log.push(`received:${frame.payload}`));
    ws.once('framereceived', frame => log.push(`once:${frame.payload}`));

    const first = ws.waitForEvent('framereceived', frame => frame.payload === 'one');
    await page.evaluate(() => (window as any).__ws.send('one'));
    expect((await first).payload).toBe('one');

    ws.off('framesent', sent);
    const second = ws.waitForEvent('framereceived', frame => frame.payload === 'two');
    await page.evaluate(() => (window as any).__ws.send('two'));
    await second;
    await expect.poll(() => log.filter(entry => entry === 'received:two').length).toBe(1);
    expect(log).toEqual(['sent:one', 'received:one', 'once:one', 'received:two']);
  });

  test('a binary frame carries its bytes', async ({ page, baseURL }) => {
    const ws = await openSocket(page, baseURL);
    const received = ws.waitForEvent('framereceived');
    await page.evaluate(() => (window as any).__ws.send(new Uint8Array([1, 2, 255])));
    const { payload } = await received;
    expect(typeof payload).not.toBe('string');
    expect(Array.from(payload as Uint8Array)).toEqual([1, 2, 255]);
  });

  test('close hands over the socket and ends waits for frames', async ({ page, baseURL }) => {
    const ws = await openSocket(page, baseURL);
    const frame = ws.waitForEvent('framereceived').then(() => 'frame', (e: Error) => e.message);
    const closed = ws.waitForEvent('close');
    await page.evaluate(() => (window as any).__ws.close());
    const socket = await closed;
    expect(socket.url()).toBe(ws.url());
    expect(ws.isClosed()).toBe(true);
    expect(await frame).toContain('Socket closed');
    expect(await ws.waitForEvent('framesent').then(() => 'frame', (e: Error) => e.message)).toContain('Socket closed');
  });
});
