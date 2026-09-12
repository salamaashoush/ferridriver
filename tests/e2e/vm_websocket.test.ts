import { test, expect } from '@ferridriver/test';

test('websocket callbacks can await a later message', async ({ page }) => {
  test.setTimeout(5000);
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.routeWebSocket('ws://example.invalid/async-callback', (ws) => {
    ws.onMessage(async (message) => {
      if (message === 'first') await gate;
      else release();
      await ws.send(message);
    });
  });
  await page.goto('/fx/landed');
  const messages = await page.evaluate(() => new Promise<string[]>((resolve) => {
    const ws = new WebSocket('ws://example.invalid/async-callback');
    const received: string[] = [];
    ws.onopen = () => { ws.send('first'); ws.send('second'); };
    ws.onmessage = (event) => {
      received.push(event.data);
      if (received.length === 2) { ws.close(); resolve(received.sort()); }
    };
  }));
  expect(messages).toEqual(['first', 'second']);
});

test('a parked websocket callback does not stall another socket', async ({ page }) => {
  test.setTimeout(5000);
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.routeWebSocket('ws://example.invalid/slow', (ws) => {
    ws.onMessage(async () => { await gate; await ws.send('released'); });
  });
  await page.routeWebSocket('ws://example.invalid/fast', (ws) => {
    ws.onMessage(async () => { release(); await ws.send('fast'); });
  });
  await page.goto('/fx/landed');
  const messages = await page.evaluate(() => new Promise<string[]>((resolve) => {
    const slow = new WebSocket('ws://example.invalid/slow');
    const received: string[] = [];
    const record = (event: MessageEvent) => {
      received.push(event.data);
      if (received.length === 2) resolve(received.sort());
    };
    slow.onmessage = record;
    slow.onopen = () => {
      slow.send('wait');
      const fast = new WebSocket('ws://example.invalid/fast');
      fast.onopen = () => fast.send('release');
      fast.onmessage = record;
    };
  }));
  expect(messages).toEqual(['fast', 'released']);
});
