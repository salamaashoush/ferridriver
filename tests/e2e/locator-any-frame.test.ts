// Ported from Playwright's tests/page/locator-any-frame.spec.ts:
// `frameLocator()` without a selector matches in whichever frame of the
// subtree the rest of the locator is found in.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';
const EMPTY_PAGE = `${ORIGIN}/any-frame/empty.html`;

function routePage(page: Page, url: string, body: string) {
  return page.route(`**/any-frame/${url}`, route => route.fulfill({ body, contentType: 'text/html' }));
}

async function waitForAllFrames(page: Page, frameCount: number, selector: string) {
  await expect.poll(() => page.frames().length).toBe(frameCount);
  for (const frame of page.frames()) {
    if (frame !== page.mainFrame())
      await frame.waitForSelector(selector, { state: 'attached' });
  }
}

test.describe('frameLocator()', () => {
  test('clicks a button inside an iframe', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<button onclick="window.__clicked = true">Click me</button>`);
    await page.goto(EMPTY_PAGE);
    await page.frameLocator().getByRole('button', { name: 'Click me' }).click();
    expect(await page.frames()[1].evaluate(() => (window as any).__clicked)).toBe(true);
  });

  test('clicks a button in the main frame', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe><button onclick="window.__clicked = true">Click me</button>`);
    await routePage(page, 'a.html', `<div>No buttons here</div>`);
    await page.goto(EMPTY_PAGE);
    await page.frameLocator().locator('button').click();
    expect(await page.evaluate(() => (window as any).__clicked)).toBe(true);
  });

  test('fails a click when elements match in multiple frames', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe><iframe src="b.html"></iframe>`);
    await routePage(page, 'a.html', `<button>one</button>`);
    await routePage(page, 'b.html', `<button>two</button>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 3, 'button');
    const started = Date.now();
    const error = await page.frameLocator().locator('button').click({ timeout: 3000 }).catch(e => e);
    expect(error.message).toContain('frameLocator() matched elements in multiple frames');
    expect(Date.now() - started).toBeLessThan(2500);
  });

  test('fails a click upon a strict mode violation inside a single frame', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<button>one</button><button>two</button>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 2, 'button');
    const error = await page.frameLocator().locator('button').click({ timeout: 3000 }).catch(e => e);
    expect(error.message).toContain('strict mode violation');
  });

  test('times out a click when nothing matches', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<div>Nothing here</div>`);
    await page.goto(EMPTY_PAGE);
    const error = await page.frameLocator().locator('button').click({ timeout: 1000 }).catch(e => e);
    expect(error.message).toContain('Timeout 1000ms exceeded');
  });

  test('counts elements in a single frame', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<div>1</div><div>2</div><div>3</div>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 2, 'div');
    expect(await page.frameLocator().locator('div').count()).toBe(3);
    expect(await page.frameLocator().locator('button').count()).toBe(0);
  });

  test('fails count when elements match in multiple frames', async ({ page }) => {
    await routePage(page, 'empty.html', `<div>main</div><iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<div>child</div>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 2, 'div');
    const error = await page.frameLocator().locator('div').count().catch(e => e);
    expect(error.message).toContain('frameLocator() matched elements in multiple frames');
  });

  test('supports toHaveCount', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<span>one</span><span>two</span>`);
    await page.goto(EMPTY_PAGE);
    await expect(page.frameLocator().locator('span')).toHaveCount(2);
    await expect(page.frameLocator().locator('button')).toHaveCount(0);
  });

  test('waits for a frame to appear with toHaveCount', async ({ page }) => {
    await routePage(page, 'empty.html', `<div>No frames yet</div>`);
    await routePage(page, 'a.html', `<span>one</span><span>two</span>`);
    await page.goto(EMPTY_PAGE);
    await page.evaluate(() => {
      setTimeout(() => {
        const iframe = document.createElement('iframe');
        iframe.src = 'a.html';
        document.body.appendChild(iframe);
      }, 500);
    });
    await expect(page.frameLocator().locator('span')).toHaveCount(2);
  });

  test('fails toHaveCount when elements match in multiple frames', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe><iframe src="b.html"></iframe>`);
    await routePage(page, 'a.html', `<span>one</span>`);
    await routePage(page, 'b.html', `<span>two</span>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 3, 'span');
    const error = await expect(page.frameLocator().locator('span')).toHaveCount(2, { timeout: 3000 }).catch(e => e);
    expect(error.message).toContain('frameLocator() matched elements in multiple frames');
    expect(error.message).toContain(`Locator: frameLocator().locator('span')`);
  });

  test('supports toHaveText', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<div>Hello iframe</div>`);
    await page.goto(EMPTY_PAGE);
    await expect(page.frameLocator().locator('div')).toHaveText('Hello iframe');
  });

  test('supports toHaveText with an array', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<span>one</span><span>two</span>`);
    await page.goto(EMPTY_PAGE);
    await expect(page.frameLocator().locator('span')).toHaveText(['one', 'two']);
  });

  test('fails toHaveText when elements match in multiple frames', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe><iframe src="b.html"></iframe>`);
    await routePage(page, 'a.html', `<div>one</div>`);
    await routePage(page, 'b.html', `<div>two</div>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 3, 'div');
    const error = await expect(page.frameLocator().locator('div')).toHaveText('one', { timeout: 3000 }).catch(e => e);
    expect(error.message).toContain('frameLocator() matched elements in multiple frames');
    expect(error.message).toContain(`Locator: frameLocator().locator('div')`);
  });

  test('fails toHaveText upon a strict mode violation inside a single frame', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<div>one</div><div>two</div>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 2, 'div');
    const error = await expect(page.frameLocator().locator('div')).toHaveText('one', { timeout: 3000 }).catch(e => e);
    expect(error.message).toContain('strict mode violation');
    expect(error.message).toContain(`Locator: frameLocator().locator('div')`);
  });

  test('supports evaluate and evaluateAll', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<div data-foo="bar">Hello</div><span>one</span><span>two</span>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 2, 'span');
    expect(await page.frameLocator().locator('div').evaluate((e: Element) => e.getAttribute('data-foo'))).toBe('bar');
    expect(await page.frameLocator().locator('span').evaluateAll((els: Element[]) => els.map(e => e.textContent))).toEqual(['one', 'two']);
    expect(await page.frameLocator().locator('button').evaluateAll((els: Element[]) => els.length)).toBe(0);
  });

  test('fails evaluate when elements match in multiple frames', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe><iframe src="b.html"></iframe>`);
    await routePage(page, 'a.html', `<div>one</div>`);
    await routePage(page, 'b.html', `<div>two</div>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 3, 'div');
    const error = await page.frameLocator().locator('div').evaluate((e: Element) => e.textContent, undefined, { timeout: 3000 }).catch((e: Error) => e) as Error;
    expect(error.message).toContain('frameLocator() matched elements in multiple frames');
    const all = await page.frameLocator().locator('div').evaluateAll((els: Element[]) => els.length).catch((e: Error) => e) as Error;
    expect(all.message).toContain('frameLocator() matched elements in multiple frames');
  });

  test('supports filters, first/last/nth and composite locators', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<div><span>one</span></div><div><span>two</span></div><div><i>three</i></div>`);
    await page.goto(EMPTY_PAGE);
    await waitForAllFrames(page, 2, 'span');
    await expect(page.frameLocator().locator('div', { hasText: 'two' })).toHaveText('two');
    await expect(page.frameLocator().locator('span').first()).toHaveText('one');
    await expect(page.frameLocator().locator('span').last()).toHaveText('two');
    await expect(page.frameLocator().locator('div').nth(1).locator('span')).toHaveText('two');
    await expect(page.frameLocator().locator('div', { has: page.locator('i') })).toHaveText('three');
  });

  test('does not match a chain across a frame boundary', async ({ page }) => {
    await routePage(page, 'empty.html', `<section><iframe src="a.html"></iframe></section>`);
    await routePage(page, 'a.html', `<iframe src="b.html"></iframe>`);
    await routePage(page, 'b.html', `<button>deep</button>`);
    await page.goto(EMPTY_PAGE);
    await expect.poll(() => page.frames().length).toBe(3);
    const deepFrame = page.frames().find(f => f.url().includes('b.html'))!;
    await deepFrame.waitForSelector('button', { state: 'attached' });
    await expect(page.frameLocator().locator('section').locator('button')).toHaveCount(0);
    await expect(page.frameLocator().locator('button')).toHaveText('deep');
  });

  test('only searches frames inside the starting frame', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe><button>main</button>`);
    await routePage(page, 'a.html', `<iframe src="b.html"></iframe>`);
    await routePage(page, 'b.html', `<button>deep</button>`);
    await page.goto(EMPTY_PAGE);
    await expect.poll(() => page.frames().length).toBe(3);
    const deepFrame = page.frames().find(f => f.url().includes('b.html'))!;
    await deepFrame.waitForSelector('button', { state: 'attached' });
    const middleFrame = page.frames().find(f => f.url().includes('a.html'))!;
    await expect(middleFrame.frameLocator().locator('button')).toHaveText('deep');
  });

  test('enters a frame found in a nested frame', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe><button>main</button>`);
    await routePage(page, 'a.html', `<iframe id="target" src="b.html"></iframe><button>decoy</button>`);
    await routePage(page, 'b.html', `<button onclick="window.__clicked = true">inside</button>`);
    await page.goto(EMPTY_PAGE);
    await expect(page.frameLocator().frameLocator('#target').locator('button')).toHaveText('inside');
    await page.frameLocator().frameLocator('#target').getByRole('button', { name: 'inside' }).click();
    const frame = page.frames().find(f => f.url().includes('b.html'))!;
    expect(await frame.evaluate(() => (window as any).__clicked)).toBe(true);
    expect(await page.frameLocator().frameLocator('#target').owner().getAttribute('id')).toBe('target');
  });

  test('does not search nested frames after entering a frame', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe id="target" src="a.html"></iframe><button>main</button>`);
    await routePage(page, 'a.html', `<iframe src="b.html"></iframe>`);
    await routePage(page, 'b.html', `<button>deep</button>`);
    await page.goto(EMPTY_PAGE);
    await expect.poll(() => page.frames().length).toBe(3);
    await expect(page.frameLocator().frameLocator('#target').locator('button')).toHaveCount(0);
  });

  test('supports two frameLocators and a locator before one', async ({ page }) => {
    await routePage(page, 'empty.html', `<iframe src="a.html"></iframe>`);
    await routePage(page, 'a.html', `<iframe id="x" src="b.html"></iframe><section><iframe src="d.html"></iframe></section>`);
    await routePage(page, 'b.html', `<iframe id="y" src="c.html"></iframe><button>decoy</button>`);
    await routePage(page, 'c.html', `<button>bottom</button>`);
    await routePage(page, 'd.html', `<button>in-section</button>`);
    await page.goto(EMPTY_PAGE);
    await expect(page.frameLocator().frameLocator('#x').frameLocator('#y').locator('button')).toHaveText('bottom');
    await expect(page.frameLocator().locator('section').frameLocator('iframe').locator('button')).toHaveText('in-section');
  });

  test('refuses to pick the nth frame and an empty selector', async ({ page }) => {
    expect(() => page.frameLocator().first()).toThrow('Selecting the nth frame is not allowed on frameLocator().');
    expect(() => page.frameLocator().nth(1)).toThrow('Selecting the nth frame is not allowed on frameLocator().');
    const error = await page.frameLocator().owner().count().catch(e => e);
    expect(error.message).toContain('Selector cannot be empty after frameLocator()');
  });

  test('prints as frameLocator()', async ({ page }) => {
    expect(String(page.frameLocator().getByText('foo').locator('span'))).toBe(`frameLocator().getByText('foo').locator('span')`);
  });
});

test.describe('locator.visible() and within()', () => {
  test('visible() keeps only the visible matches', async ({ page }) => {
    await page.setContent(`<button style="display:none">hidden</button><button>shown</button>`);
    await expect(page.locator('button').visible()).toHaveText('shown');
    expect(await page.locator('button').visible().count()).toBe(1);
    expect(String(page.locator('button').visible())).toBe(`locator('button').visible()`);
  });

  test('within() searches inside the scope', async ({ page }) => {
    await page.setContent(`<section><p>inside</p></section><p>outside</p>`);
    const inside = page.locator('p').within(page.locator('section'));
    await expect(inside).toHaveText('inside');
    expect(String(inside)).toBe(`locator('section').locator(locator('p'))`);
  });
});

test.describe('page.getByRef()', () => {
  test('finds the element an AI snapshot labelled', async ({ page }) => {
    await page.setContent(`<button id="submit">Submit</button><a href="#">Read more</a>`);
    const snapshot = await page.locator('body').ariaSnapshot({ mode: 'ai' });
    const ref = /button "Submit" \[ref=(e\d+)\]/.exec(snapshot)?.[1];
    expect(ref).toBeTruthy();
    await expect(page.getByRef(ref!)).toHaveId('submit');
    expect(String(page.getByRef(ref!))).toBe(`locator('aria-ref=${ref}')`);
  });
});
