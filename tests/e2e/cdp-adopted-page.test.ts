import { test, expect } from '@ferridriver/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

for (const hostname of ['127.0.0.1', 'localhost']) {
test(`attaching an existing ${hostname === 'localhost' ? 'cross-process' : 'same-process'} frame retains its document and ownership`, async () => {
  const owner = await chromium({transport: 'ws'}).launch({headless: true});
  try {
    const existing = await owner.newPage();
    await existing.goto('http://127.0.0.1:47831/fx/landed');
    await existing.evaluate((hostname: string) => {
      const frame = document.createElement('iframe');
      frame.name = 'existing-remote';
      frame.src = `http://${hostname}:47831/fx/landed`;
      document.body.appendChild(frame);
      return new Promise(resolve => frame.onload = resolve);
    }, hostname);
    const protocol = await owner.newBrowserCDPSession();
    const commandLine = await protocol.send('Browser.getBrowserCommandLine') as {arguments: string[]};
    const profile = commandLine.arguments.find(arg => arg.startsWith('--user-data-dir='))!.slice('--user-data-dir='.length);
    const [port] = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n');
    await protocol.detach();
    const attached = await chromium().connectOverCDP(`http://127.0.0.1:${port}`);
    try {
      const pages = await attached.contexts()[0].pages();
      const page = pages.find(page => page.url() === existing.url());
      expect(Boolean(page)).toBe(true);
      const frame = page!.frame('existing-remote');
      expect(Boolean(frame)).toBe(true);
      expect(await frame!.evaluate(() => location.hostname)).toBe(hostname);
      expect(frame!.parentFrame()!.url()).toBe(page!.url());
    } finally {
      await attached.close();
    }
    expect(existing.isClosed()).toBe(false);
    expect(await existing.frame('existing-remote')!.evaluate(() => location.hostname)).toBe(hostname);
  } finally {
    await owner.close();
  }
});

}

test('an attached existing tab has a main frame and completes navigation', async () => {
  const owner = await chromium({ transport: 'ws' }).launch({ headless: true });
  try {
    await owner.newPage();
    const protocol = await owner.newBrowserCDPSession();
    const commandLine = await protocol.send('Browser.getBrowserCommandLine') as { arguments: string[] };
    const profileArg = commandLine.arguments.find(arg => arg.startsWith('--user-data-dir='));
    expect(profileArg).toBeTruthy();
    const profile = profileArg!.slice('--user-data-dir='.length);
    const [port] = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n');
    await protocol.detach();
    const browser = await chromium().connectOverCDP(`http://127.0.0.1:${port}`);
    try {
      const [page] = await browser.contexts()[0].pages();
      expect(page).toBeTruthy();
      expect(await page.locator('body').count()).toBe(1);
      const html = '<input id="name"><button onclick="document.title=document.querySelector(\'#name\').value">Save</button>';
      await page.goto(`data:text/html,${encodeURIComponent(html)}`, { timeout: 2000 });
      await page.locator('#name').fill('sashoush');
      await page.getByRole('button', { name: 'Save' }).click();
      expect(await page.title()).toBe('sashoush');
    } finally {
      await browser.close();
    }
  } finally {
    await owner.close();
  }
});
