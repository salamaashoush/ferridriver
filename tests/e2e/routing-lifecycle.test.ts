import { test, expect } from '@ferridriver/test';

test('parallel pages can repeatedly route and unroute through another page wrapper', async ({ context, baseURL }) => {
  await Promise.all(Array.from({ length: 6 }, async (_, index) => {
    const page = await context.newPage();
    try {
      await page.goto(`${baseURL}/fx/landed`);
      const predicate = (url: URL) => url.pathname === '/fx/api/users';
      for (let cycle = 0; cycle < 4; cycle++) {
        await test.step(`page ${index}, routing cycle ${cycle}`, async () => {
          const marker = `page-${index}-cycle-${cycle}`;
          await page.route(predicate, route => {
            route.fulfill({ status: 200, contentType: 'text/plain', body: marker });
          });
          expect(await page.evaluate("fetch('/fx/api/users').then(response => response.text())")).toBe(marker);
          await page.mainFrame().page().unroute(predicate);
          const restored = await page.evaluate("fetch('/fx/api/users').then(response => response.text())");
          expect(String(restored)).toContain('alice');
        });
      }
    } finally {
      await page.unrouteAll();
      await page.close();
    }
  }));
});
