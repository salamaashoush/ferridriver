import {test, expect} from '@ferridriver/test';

for (const transport of ['pipe', 'ws'] as const) {
  test(`remote renderer inherits scripts and later routes over ${transport}`, async () => {
    const browser = await chromium({transport}).launch({headless: true});
    try {
      const page = await browser.newPage();
      await page.addInitScript(() => { (window as any).rendererInit = 42; });
      await page.goto('http://127.0.0.1:47831/fx/landed');
      await page.evaluate(() => {
        const frame = document.createElement('iframe');
        frame.name = 'remote';
        frame.src = 'http://localhost:47831/fx/landed';
        document.body.appendChild(frame);
        return new Promise(resolve => frame.onload = resolve);
      });
      const remote = page.frame('remote');
      expect(Boolean(remote)).toBe(true);
      expect(Number(await remote!.evaluate(() => (window as any).rendererInit))).toBe(42);
      await page.exposeFunction('describeOwner', (name: string) => `owner: ${name}`);
      expect(await remote!.evaluate(() => (window as any).describeOwner('sashoush'))).toBe('owner: sashoush');
      await page.route('**/renderer-data', route => route.fulfill({body: 'sashoush'}));
      expect(await remote!.evaluate(() => fetch('/renderer-data').then(response => response.text()))).toBe('sashoush');
      await page.addInitScript(() => { (window as any).laterInit = 'sashoush'; });
      await page.evaluate(() => {
        const frame = document.querySelector('iframe[name="remote"]') as HTMLIFrameElement;
        const loaded = new Promise(resolve => frame.onload = resolve);
        frame.src = 'http://localhost:47831/fx/landed?new-document';
        return loaded;
      });
      expect(remote!.isDetached()).toBe(false);
      expect(await remote!.evaluate(() => (window as any).laterInit)).toBe('sashoush');
      expect(await remote!.evaluate(() => (window as any).describeOwner('sashoush'))).toBe('owner: sashoush');
      await page.unroute('**/renderer-data');
      expect(await remote!.evaluate(() => fetch('/fx/landed').then(response => response.status))).toBe(200);
      await remote!.evaluate(() => {
        const frame = document.createElement('iframe');
        frame.name = 'nested-remote';
        frame.src = 'http://127.0.0.1:47831/fx/landed';
        document.body.appendChild(frame);
        return new Promise(resolve => frame.onload = resolve);
      });
      const nested = page.frame('nested-remote');
      expect(Boolean(nested)).toBe(true);
      expect(Number(await nested!.evaluate(() => (window as any).rendererInit))).toBe(42);
      await page.evaluate(() => document.querySelector('iframe[name="remote"]')!.remove());
      expect(remote!.isDetached()).toBe(true);
      expect(nested!.isDetached()).toBe(true);
      await page.addInitScript(() => { (window as any).afterRemoval = true; });
      await page.route('**/after-removal', route => route.fulfill({body: 'still usable'}));
      expect(await page.evaluate(() => fetch('/after-removal').then(response => response.text()))).toBe('still usable');
    } finally {
      await browser.close();
    }
  });
}
