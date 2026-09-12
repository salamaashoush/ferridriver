import { test, expect } from '@ferridriver/test';

test('a lazy context lists no pages until its first page is opened', async ({ browser }) => {
  const context = await browser.newContext();
  try {
    expect(await context.pages()).toEqual([]);
    const page = await context.newPage();
    await page.setContent('<title>sashoush</title>');
    const pages = await context.pages();
    expect(pages.length).toBe(1);
    expect(await pages[0].title()).toBe('sashoush');
  } finally {
    await context.close();
  }
});
