import { test, expect } from '@ferridriver/test';

test('history navigation and reload refresh the current frame tree', async ({ page, baseURL }) => {
  const framed = `${baseURL}/fx/srcdoc`;
  const empty = `${baseURL}/fx/landed`;
  await page.goto(framed);
  await page.goto(empty);
  await page.goBack();
  expect(page.url()).toBe(framed);
  expect(page.frames().length).toBe(2);
  expect(await page.frame('child')!.locator('p').textContent()).toBe('child');
  await page.reload();
  const child = page.frame('child');
  expect(page.frames().length).toBe(2);
  expect(await child!.locator('p').textContent()).toBe('child');
  await page.goForward();
  expect(page.url()).toBe(empty);
  expect(page.frames().length).toBe(1);
  expect(child!.isDetached()).toBe(true);
});

test('navigation refreshes named frames and retires the previous document frames', async ({ page }) => {
  await page.goto('data:text/html,' + encodeURIComponent('<iframe name="child" srcdoc="<p>child</p>"></iframe>'));
  expect(page.frames().length).toBe(2);
  const child = page.frame('child');
  expect(child).not.toBeNull();
  expect(child!.name()).toBe('child');
  expect(child!.url()).toBe('about:srcdoc');
  expect(await child!.locator('p').textContent()).toBe('child');
  await page.goto('data:text/html,<title>No frames</title>');
  expect(page.frames().length).toBe(1);
  expect(child!.isDetached()).toBe(true);
  expect(page.frame('child')).toBeNull();
});

test('baseURL preserves document paths for query and fragment references', async ({ browser, baseURL }) => {
  const context = await browser.newContext({baseURL: `${baseURL}/fx/landed?original=1#old`});
  try {
    const page = await context.newPage();
    for (const [given, expected] of [
      ['?next=2', `${baseURL}/fx/landed?next=2`],
      ['#after', `${baseURL}/fx/landed?original=1#after`],
      ['', `${baseURL}/fx/landed?original=1`],
      ['../fx/landed', `${baseURL}/fx/landed`],
    ]) {
      await page.goto(given);
      expect(page.url()).toBe(expected);
    }
  } finally {
    await context.close();
  }
});

test('invalid navigation leaves the current document usable', async ({ page }) => {
  await page.goto('data:text/html,<title>Preserved page</title>');
  const before = page.url();
  await expect(page.goto('http://[')).rejects.toThrow();
  expect(page.url()).toBe(before);
  expect(await page.title()).toBe('Preserved page');
});

test('navigation completes a local address without a scheme', async ({ browser, baseURL }) => {
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    const target = `${baseURL}/fx/landed`;
    await page.goto(target.replace(/^http:\/\//, ''));
    expect(page.url()).toBe(target);
  } finally {
    await context.close();
  }
});

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
