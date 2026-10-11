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

// Playwright: `browserContext.pages(): Page[]` is synchronous
// (client/browserContext.ts), so `context.pages().length` and
// `const [page] = context.pages()` read the list without an await.
test('pages() answers at once, with the pages a script already holds', async ({ browser }) => {
  const context = await browser.newContext();
  try {
    expect(Array.isArray(context.pages())).toBe(true);
    expect(context.pages().length).toBe(0);
    const first = await context.newPage();
    const second = await context.newPage();
    const [a, b] = context.pages();
    expect([a === first, b === second, context.pages().length]).toEqual([true, true, 2]);

    await first.close();
    expect(context.pages().length).toBe(1);
    expect(context.pages()[0] === second).toBe(true);
    // The handle a page gives of its own context lists the same pages.
    expect(second.context().pages().length).toBe(1);
  } finally {
    await context.close();
  }
  expect(context.pages()).toEqual([]);
});

test('pages() lists a popup once it is announced', async ({ context, page, baseURL }) => {
  await page.goto(`${baseURL}/fx/landed`);
  expect(context.pages().length).toBe(1);
  const [popup] = await Promise.all([context.waitForEvent('page'), page.evaluate(() => void window.open('about:blank'))]);
  expect(context.pages().length).toBe(2);
  expect(context.pages()[1] === popup).toBe(true);
  await (popup as { close(): Promise<void> }).close();
  expect(context.pages().length).toBe(1);
});

test('discoverPages() resolves to the same list', async ({ context, page }) => {
  const discovered = await context.discoverPages();
  expect(discovered.length).toBe(1);
  expect(discovered[0] === page).toBe(true);
});
