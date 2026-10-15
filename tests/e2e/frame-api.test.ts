// Playwright's Frame surface beyond locators: handles, navigation, waits
// and content, each on a child frame, where a method that quietly ran
// against the main frame would give a different answer.
import { test, expect } from '@ferridriver/test';
import type { Frame, Page } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';
const HOST = `${ORIGIN}/frames/one-frame.html`;
const EMPTY = `${ORIGIN}/empty.html`;

async function childOf(page: Page): Promise<Frame> {
  await page.goto(HOST);
  const frame = page.frame('child');
  expect(frame).not.toBeNull();
  await frame!.waitForLoadState();
  return frame!;
}

const outcome = (work: Promise<unknown>) => work.then(() => 'resolved', (e: Error) => String(e));

test.describe('frame handles', () => {
  test('$ and $$ look inside their own frame', async ({ page }) => {
    const frame = await childOf(page);
    const button = await frame.$('#frame-button');
    expect(await button!.textContent()).toBe('Click in frame');
    expect(await frame.$('#missing')).toBeNull();
    expect((await frame.$$('input, button')).length).toBe(2);

    // The same selectors from the main frame, which holds only the iframe.
    expect(await page.mainFrame().$('#frame-button')).toBeNull();
    expect((await page.mainFrame().$$('input, button')).length).toBe(0);
    expect((await page.mainFrame().$$('iframe')).length).toBe(1);
  });

  test('$ returns the first match unless strict', async ({ page }) => {
    const frame = await childOf(page);
    const first = await frame.$('input, button');
    expect(await first!.evaluate((e: Element) => e.id)).toBe('frame-input');
    expect(await outcome(frame.$('input, button', { strict: true }))).toContain('strict mode violation');
    expect(await (await frame.$('#frame-input', { strict: true }))!.evaluate((e: Element) => e.id)).toBe('frame-input');

    await page.setContent('<p>one</p><p>two</p>');
    expect(await (await page.$('p'))!.textContent()).toBe('one');
    expect(await outcome(page.$('p', { strict: true }))).toContain('strict mode violation');
    expect(await outcome(page.$('nosuchengine=x'))).toContain('unknown selector engine');
  });

  test('frameElement is the iframe holding the frame', async ({ page }) => {
    const frame = await childOf(page);
    const element = await frame.frameElement();
    expect(await element.evaluate((e: Element) => `${e.tagName}:${e.getAttribute('name')}`)).toBe('IFRAME:child');
    expect((await element.contentFrame())!.name()).toBe('child');
    expect(await outcome(page.mainFrame().frameElement())).toContain('Frame has been detached.');
  });
});

test.describe('frame navigation', () => {
  test('goto navigates the child frame and answers with its response', async ({ page }) => {
    const frame = await childOf(page);
    const response = await frame.goto(EMPTY);
    expect(response!.status()).toBe(200);
    expect(response!.url()).toBe(EMPTY);
    expect(frame.url()).toBe(EMPTY);
    expect(await frame.title()).toBe('Empty');
    expect(page.url()).toBe(HOST);
    expect(await page.title()).toBe('One Frame');

    expect(await outcome(frame.goto(HOST, { referer: 'http://example.com/' }))).toContain('referer');
    // The main frame is the page.
    const top = await page.mainFrame().goto(EMPTY, { waitUntil: 'domcontentloaded' });
    expect(top!.status()).toBe(200);
    expect(page.url()).toBe(EMPTY);
  });

  test('waitForNavigation hears the next navigation of its frame', async ({ page }) => {
    const frame = await childOf(page);
    const [response] = await Promise.all([
      frame.waitForNavigation({ url: /empty\.html$/ }),
      frame.evaluate((url: string) => { location.href = url; }, EMPTY),
    ]);
    expect(response!.status()).toBe(200);
    expect(response!.url()).toBe(EMPTY);
    expect(frame.url()).toBe(EMPTY);
    expect(page.url()).toBe(HOST);

    // The child navigating is not a navigation of the page.
    const [, missed] = await Promise.all([
      frame.goto(`${ORIGIN}/frames/frame.html`),
      outcome(page.waitForNavigation({ timeout: 700 })),
    ]);
    expect(missed).toContain('Timeout');
  });

  test('page.waitForNavigation gives the document response, or null within a document', async ({ page }) => {
    await page.goto(EMPTY);
    const [response] = await Promise.all([
      page.waitForNavigation({ waitUntil: 'commit' }),
      page.evaluate((url: string) => { location.href = url; }, HOST),
    ]);
    expect(response!.status()).toBe(200);
    expect(response!.url()).toBe(HOST);
    expect(page.url()).toBe(HOST);

    const [within] = await Promise.all([
      page.waitForNavigation(),
      page.evaluate(() => { history.pushState({}, '', '#section'); }),
    ]);
    expect(within).toBeNull();
    expect(page.url()).toBe(`${HOST}#section`);
  });

  test('waitForURL follows the frame URL and not the page URL', async ({ page }) => {
    const frame = await childOf(page);
    await Promise.all([
      frame.waitForURL('**/empty.html'),
      frame.evaluate((url: string) => { setTimeout(() => { location.href = url; }, 100); }, EMPTY),
    ]);
    expect(frame.url()).toBe(EMPTY);
    await frame.waitForURL(/empty/, { waitUntil: 'domcontentloaded' });
    // The page is still where it was, so its own wait for that URL runs out.
    expect(await outcome(page.waitForURL('**/empty.html', { timeout: 500 }))).toContain('Timeout');
    expect(await outcome(frame.waitForURL('**/never.html', { timeout: 500 }))).toContain('Timeout');
  });

  test('waitForLoadState is about the frame document', async ({ page }) => {
    const frame = await childOf(page);
    const key = `frame-load-${Date.now()}`;
    await frame.goto(`${ORIGIN}/fx/control/page/${key}`, { waitUntil: 'domcontentloaded' });
    await frame.waitForLoadState('domcontentloaded');
    expect(await outcome(frame.waitForLoadState('load', { timeout: 500 }))).toContain('Timeout');
    expect(await outcome(frame.waitForLoadState(undefined, { timeout: 500 }))).toContain('Timeout');
    await page.request.get(`${ORIGIN}/fx/control/release/${key}`);
    await frame.waitForLoadState('load');
    expect(await frame.evaluate(() => document.readyState)).toBe('complete');
  });
});

test.describe('frame content', () => {
  test('setContent rewrites the child document and runs its scripts', async ({ page }) => {
    const frame = await childOf(page);
    await frame.setContent('<title>Written</title><script>window.ran = (window.ran || 0) + 1;</script><p id="p">hi</p>');
    expect(await frame.evaluate(() => (window as any).ran)).toBe(1);
    expect(await frame.title()).toBe('Written');
    expect(await frame.textContent('#p')).toBe('hi');
    expect(await page.title()).toBe('One Frame');

    await frame.setContent('<p id="p">again</p>', { waitUntil: 'commit' });
    expect(await frame.textContent('#p')).toBe('again');

    // A document held before load never reaches the default `load`.
    const key = `set-content-${Date.now()}`;
    const held = `<p id="p">held</p><img src="${ORIGIN}/fx/control/hold/${key}">`;
    expect(await outcome(frame.setContent(held, { timeout: 500 }))).toContain('Timeout');
    await frame.setContent(held, { waitUntil: 'domcontentloaded' });
    expect(await frame.textContent('#p')).toBe('held');
    await page.request.get(`${ORIGIN}/fx/control/release/${key}`);

    await page.setContent('<title>Top</title><p>top</p>', { waitUntil: 'domcontentloaded' });
    expect(await page.title()).toBe('Top');
  });

  test('addScriptTag returns the script it added to the frame', async ({ page }) => {
    const frame = await childOf(page);
    const inline = await frame.addScriptTag({ content: 'window.__inline = 42;' });
    expect(await inline.evaluate((e: Element) => `${e.tagName}:${(e as HTMLScriptElement).type}`)).toBe('SCRIPT:text/javascript');
    expect(await frame.evaluate(() => (window as any).__inline)).toBe(42);
    expect(await page.evaluate(() => (window as any).__inline)).toBeUndefined();

    const external = await frame.addScriptTag({ url: `${ORIGIN}/frames/tag.js` });
    expect(await external.evaluate((e: Element) => (e as HTMLScriptElement).src)).toBe(`${ORIGIN}/frames/tag.js`);
    expect(await frame.evaluate(() => (window as any).__fromFile)).toBe('file-ok');

    await frame.evaluate(() => { delete (window as any).__fromFile; });
    const fromPath = await frame.addScriptTag({ path: 'tests/assets/frames/tag.js' });
    expect(await fromPath.evaluate((e: Element) => (e as HTMLScriptElement).text)).toContain('//# sourceURL=tests/assets/frames/tag.js');
    expect(await frame.evaluate(() => (window as any).__fromFile)).toBe('file-ok');

    const module = await frame.addScriptTag({ content: 'window.__module = typeof import.meta;', type: 'module' });
    expect(await module.evaluate((e: Element) => (e as HTMLScriptElement).type)).toBe('module');
    await expect.poll(() => frame.evaluate(() => (window as any).__module)).toBe('object');

    expect(await outcome(frame.addScriptTag())).toContain('Provide an object with a `url`, `path` or `content` property');
    expect(await outcome(frame.addScriptTag({ url: `${ORIGIN}/fx/status/404` }))).toContain('Failed to load script');

    const onPage = await page.addScriptTag({ content: 'window.__top = 7;' });
    expect(await onPage.evaluate((e: Element) => e.ownerDocument.title)).toBe('One Frame');
    expect(await page.evaluate(() => (window as any).__top)).toBe(7);
  });

  test('addStyleTag returns the stylesheet it added to the frame', async ({ page }) => {
    const frame = await childOf(page);
    const margin = (side: string) =>
      frame.evaluate((s: string) => getComputedStyle(document.body).getPropertyValue(s), side);
    const inline = await frame.addStyleTag({ content: 'body { margin-left: 41px; }' });
    expect(await inline.evaluate((e: Element) => e.tagName)).toBe('STYLE');
    expect(await margin('margin-left')).toBe('41px');

    const external = await frame.addStyleTag({ url: `${ORIGIN}/frames/tag.css` });
    expect(await external.evaluate((e: Element) => `${e.tagName}:${(e as HTMLLinkElement).rel}`)).toBe('LINK:stylesheet');
    expect(await margin('margin-top')).toBe('37px');

    const fromPath = await frame.addStyleTag({ path: 'tests/assets/frames/tag.css' });
    expect(await fromPath.evaluate((e: Element) => e.textContent)).toContain('/*# sourceURL=tests/assets/frames/tag.css*/');

    expect(await outcome(frame.addStyleTag())).toContain('Provide an object with a `url`, `path` or `content` property');
    const onPage = await page.addStyleTag({ content: 'body { margin-left: 13px; }' });
    expect(await onPage.evaluate((e: Element) => e.ownerDocument.title)).toBe('One Frame');
    expect(await page.evaluate(() => getComputedStyle(document.body).marginLeft)).toBe('13px');
  });

  test('waitForFunction and waitForTimeout belong to the frame', async ({ page }) => {
    const frame = await childOf(page);
    const handle = await frame.waitForFunction(() => document.querySelector('#frame-button') && window.name);
    expect(await handle.jsonValue()).toBe('child');
    await Promise.all([
      frame.waitForFunction((want: string) => document.title === want, 'Later', { polling: 50 }),
      frame.evaluate(() => { setTimeout(() => { document.title = 'Later'; }, 150); }),
    ]);
    expect(await frame.title()).toBe('Later');
    expect(await outcome(frame.waitForFunction(() => false, undefined, { timeout: 300 }))).toContain('Timeout');

    const started = Date.now();
    await frame.waitForTimeout(150);
    expect(Date.now() - started).toBeGreaterThanOrEqual(140);
  });
});
