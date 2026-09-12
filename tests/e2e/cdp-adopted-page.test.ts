import { test, expect } from '@ferridriver/test';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';

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
