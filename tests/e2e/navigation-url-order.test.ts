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
  // Sixteen contexts at once is the point; a 3-vCPU runner needs longer than 30s for them.
  test.slow();
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

test('history steps between entries of one document resolve at once with no response', async ({ page, baseURL }) => {
  const landed = `${baseURL}/fx/landed`;
  await page.goto(landed);
  await page.evaluate(() => {
    history.pushState({}, '', '?tab=two');
    history.pushState({}, '', '?tab=three');
  });
  const started = Date.now();
  expect(await page.goBack({ timeout: 5000 })).toBeNull();
  expect(page.url()).toBe(`${landed}?tab=two`);
  expect(await page.goBack({ timeout: 5000 })).toBeNull();
  expect(page.url()).toBe(landed);
  expect(await page.goForward({ timeout: 5000 })).toBeNull();
  expect(page.url()).toBe(`${landed}?tab=two`);
  expect(await page.goForward({ timeout: 5000 })).toBeNull();
  expect(page.url()).toBe(`${landed}?tab=three`);
  expect(Date.now() - started).toBeLessThan(4000);
});

test('a history step across documents still waits for the document it lands on', async ({ page, baseURL }) => {
  const landed = `${baseURL}/fx/landed`;
  const framed = `${baseURL}/fx/srcdoc`;
  await page.goto(landed);
  await page.evaluate(() => history.pushState({}, '', '?tab=two'));
  await page.goto(framed);
  expect((await page.goBack())!.url()).toBe(`${landed}?tab=two`);
  expect(await page.goBack()).toBeNull();
  expect(page.url()).toBe(landed);
  expect(await page.goForward()).toBeNull();
  expect((await page.goForward())!.url()).toBe(framed);
  expect(page.frames().length).toBe(2);
});

// The URL moves with the frame events in wire order. Thousands of console
// messages ahead of the pushState keep the page's event listener busy
// well after the evaluate returns, so a URL updated by that listener is
// still the old one when it is read.
test('page.url() reads a pushState the moment the evaluate that made it returns', async ({ page, baseURL }) => {
  await page.goto('/fx/landed');
  await page.evaluate(() => {
    for (let i = 0; i < 3000; i++) console.log(i);
    history.pushState({}, '', '/pushed');
  });
  expect(page.url()).toBe(`${baseURL}/pushed`);
  await page.evaluate(() => {
    for (let i = 0; i < 3000; i++) console.log(i);
    location.hash = 'after';
  });
  expect(page.mainFrame().url()).toBe(`${baseURL}/pushed#after`);
});

test('goto to a fragment of the loaded document resolves at once with no response', async ({ page, baseURL }) => {
  const landed = `${baseURL}/fx/landed`;
  await page.goto(landed);
  await page.evaluate(() => {
    (window as any).__kept = 1;
  });
  const started = Date.now();
  expect(await page.goto(`${landed}#first`)).toBeNull();
  expect(await page.goto(`${landed}#second`)).toBeNull();
  expect(Date.now() - started).toBeLessThan(5000);
  expect(page.url()).toBe(`${landed}#second`);
  expect(await page.evaluate(() => (window as any).__kept)).toBe(1);
});

test('goto to a document that issues no request does not wait for one', async ({ page, baseURL }) => {
  test.fixme(process.platform === 'win32' && !!process.env.CI, 'about:blank gotos run past the bound on hosted Windows runners');
  await page.goto(`${baseURL}/fx/landed`);
  // Only the about:blank gotos are timed: waiting for a request each would
  // add 250ms apiece, while the HTTP navigations between them are as slow
  // as the machine is loaded.
  let blank = 0;
  for (let i = 0; i < 5; i++) {
    const started = Date.now();
    expect(await page.goto('about:blank')).toBeNull();
    blank += Date.now() - started;
    expect(page.url()).toBe('about:blank');
    expect((await page.goto(`${baseURL}/fx/landed`))?.status()).toBe(200);
  }
  console.log(`about:blank x5 ${blank}ms`);
  expect(blank).toBeLessThan(1000);
});
