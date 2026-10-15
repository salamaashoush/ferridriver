// browser.on('disconnected') and the contexts that close with it: on
// close() for every browser, and when the browser goes away on its own.
// Each object delivers its own events, so the two are not ordered here.
// Ported from Playwright's browser.spec.ts and browsertype-connect.spec.ts.
import { test, expect } from '@ferridriver/test';
import type { Browser, BrowserContext } from '@ferridriver/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { runOnceAcrossProjects } from './helpers/projects';

function launch(browserName: string): Promise<Browser> {
  const factory = browserName === 'firefox' ? firefox() : browserName === 'webkit' ? webkit() : chromium();
  return factory.launch({ headless: true });
}

async function watch(browser: Browser, context: BrowserContext) {
  const seen: string[] = [];
  browser.on('disconnected', arg => seen.push(`disconnected:${(arg as Browser).isConnected()}`));
  context.on('close', () => seen.push('context close'));
  return seen;
}

test('close() disconnects once and closes the contexts', async ({ browserName }) => {
  const browser = await launch(browserName);
  const context = await browser.newContext();
  await context.newPage();
  const seen = await watch(browser, context);
  const disconnected = browser.waitForEvent('disconnected');
  expect(browser.isConnected()).toBe(true);
  await browser.close();
  await disconnected;
  expect(browser.isConnected()).toBe(false);
  await browser.close();
  await expect.poll(() => seen.length).toBe(2);
  expect(seen.sort()).toEqual(['context close', 'disconnected:false']);
});

test('a browser that crashes disconnects', async () => {
  runOnceAcrossProjects();
  const browser = await chromium().launch({ headless: true });
  const context = await browser.newContext();
  await context.newPage();
  const seen = await watch(browser, context);
  const disconnected = browser.waitForEvent('disconnected');
  const protocol = await browser.newBrowserCDPSession();
  await protocol.send('Browser.crash').catch(() => null);
  await disconnected;
  expect(browser.isConnected()).toBe(false);
  await expect.poll(() => seen.length).toBe(2);
  expect(seen.sort()).toEqual(['context close', 'disconnected:false']);
  await browser.close();
});

test('a connection whose browser closes elsewhere disconnects', async () => {
  runOnceAcrossProjects();
  const owner = await chromium({ transport: 'ws' }).launch({ headless: true });
  try {
    const protocol = await owner.newBrowserCDPSession();
    const commandLine = await protocol.send('Browser.getBrowserCommandLine') as { arguments: string[] };
    const profile = commandLine.arguments.find(arg => arg.startsWith('--user-data-dir='))!.slice('--user-data-dir='.length);
    const [port] = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n');
    await protocol.detach();
    const attached = await chromium().connectOverCDP(`http://127.0.0.1:${port}`);
    const disconnected = attached.waitForEvent('disconnected');
    await owner.close();
    await disconnected;
    expect(attached.isConnected()).toBe(false);
    await attached.close();
  } finally {
    await owner.close();
  }
});
