import { test, expect } from '@ferridriver/test';

test('navigation completes with the current page and main-frame URL', async ({ browser }) => {
  await Promise.all(Array.from({ length: 16 }, async (_, index) => {
    const context = await browser.newContext();
    try {
      const page = await context.newPage();
      for (let navigation = 0; navigation < 4; navigation++) {
        const url = `data:text/html,${encodeURIComponent(`<title>${index}:${navigation}</title><p>landed</p>`)}#section-${navigation}`;
        await page.goto(url);
        expect(page.url()).toBe(url);
        expect(page.mainFrame().url()).toBe(url);
      }
    } finally {
      await context.close();
    }
  }));
});
