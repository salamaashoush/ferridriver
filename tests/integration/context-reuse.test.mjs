import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer, passed, run, workspace } from './support.mjs';

const config = (reuse) => `[test]
testMatch = ["*.spec.ts"]
workers = 1
${reuse ? 'contextReuse = true' : ''}
[test.browser]
browser = "chromium"
backend = "cdp-pipe"
headless = true
`;

// Tests of one file run in order on the one worker, so each sees what the
// one before it left behind, if anything was.
const spec = (origin) => `import { test, expect, selectors } from '@ferridriver/test';
const LANDED = '${origin}/fx/landed';

async function identity(page, context) {
  const cdp = await context.newCDPSession(page);
  const { targetInfo } = await cdp.send('Target.getTargetInfo');
  await cdp.detach();
  return targetInfo.targetId;
}

async function state(page) {
  await page.goto(LANDED);
  return page.evaluate(async () => ({
    cookie: document.cookie,
    local: localStorage.getItem('kept'),
    session: sessionStorage.getItem('kept'),
    databases: (await indexedDB.databases()).length,
    name: window.name,
    initScript: (window as any).__fromInitScript ?? null,
    binding: typeof (window as any).leaked,
    geolocation: (await navigator.permissions.query({ name: 'geolocation' })).state,
    dark: matchMedia('(prefers-color-scheme: dark)').matches,
    width: innerWidth,
    online: navigator.onLine,
    userAgent: navigator.userAgent.includes('previous-test'),
  }));
}

let fresh;

test('a new context starts empty', async ({ page, context }) => {
  console.log('TARGET first ' + await identity(page, context));
  fresh = await state(page);
  expect(fresh.cookie).toBe('');
  expect(fresh.local).toBeNull();
  expect(fresh.binding).toBe('undefined');
});

test('a test leaves everything it can behind', async ({ page, context }) => {
  console.log('TARGET leaver ' + await identity(page, context));
  await context.addInitScript(() => { (window as any).__fromInitScript = 'context'; });
  await page.addInitScript(() => { (window as any).__fromInitScript = 'page'; });
  await page.exposeFunction('leaked', () => 1);
  await context.grantPermissions(['geolocation']);
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.setViewportSize({ width: 500, height: 400 });
  await page.setExtraHTTPHeaders({ 'x-previous-test': '1' });
  await context.route('**/fx/landed', (route) => route.continue({ headers: { ...route.request().headers(), 'user-agent': 'previous-test' } }));
  await page.goto(LANDED);
  await page.evaluate(async () => {
    document.cookie = 'kept=1; max-age=3600';
    localStorage.setItem('kept', '1');
    sessionStorage.setItem('kept', '1');
    window.name = 'kept';
    history.pushState({}, '', '/fx/landed?pushed');
    await new Promise((resolve, reject) => {
      const open = indexedDB.open('kept', 1);
      open.onsuccess = () => { open.result.close(); resolve(null); };
      open.onerror = () => reject(open.error);
    });
  });
  const popup = await context.newPage();
  await popup.goto(LANDED);
  console.log('TARGET popup ' + await identity(popup, context));
  await page.mouse.move(40, 40);
  await context.setOffline(true);
  expect((await context.cookies()).length).toBe(1);
});

test('the next test sees none of it', async ({ page, context }) => {
  console.log('TARGET next ' + await identity(page, context));
  expect(context.pages().length).toBe(1);
  expect(page.url()).toBe('about:blank');
  expect(await context.cookies()).toEqual([]);
  expect(await state(page)).toEqual(fresh);
  expect(await page.goBack()).toBeNull();
});

// A zone covering the viewport's corner that counts the pointer events
// it is sent.
const ZONE = '<style>body{margin:0}#zone{width:300px;height:300px}</style><div id="zone"></div>' +
  '<script>window.pointer = 0; for (const type of ["mouseover", "mouseenter", "mousemove"]) ' +
  'document.getElementById("zone").addEventListener(type, () => window.pointer++);</script>';
const frames = (page) => page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));

test('a test leaves the pointer over an element', async ({ page }) => {
  await page.setContent(ZONE);
  await page.mouse.move(150, 150);
  await frames(page);
  expect(await page.evaluate(() => (window as any).pointer)).toBeGreaterThan(0);
});

test('the next test does not open with the pointer there', async ({ page }) => {
  await page.setContent(ZONE);
  await frames(page);
  expect(await page.evaluate(() => (window as any).pointer)).toBe(0);
});

// Registered once the previous test's context has been emptied and before
// this one's page exists, so the page it is handed carries an engine
// source that no longer matches.
test.describe('an engine registered late', () => {
  test.beforeAll(async () => {
    await selectors.register('bytag', () => ({
      query(root, selector) { return root.querySelector(selector); },
      queryAll(root, selector) { return Array.from(root.querySelectorAll(selector)); },
    }));
  });

  test('resolves in the blank document and after a navigation', async ({ page }) => {
    expect(page.url()).toBe('about:blank');
    await page.setContent('<main>found</main>');
    expect(await page.locator('bytag=main').textContent()).toBe('found');
    await page.goto(LANDED);
    expect(await page.locator('bytag=body').count()).toBe(1);
  });
});

test('a test that makes a context behind a proxy nothing listens on', async ({ browser }) => {
  const context = await browser.newContext({ proxy: { server: 'http://127.0.0.1:9', bypass: '<-loopback>' } });
  const page = await context.newPage();
  let failure = '';
  await page.goto(LANDED).catch((error) => { failure = String(error); });
  expect(failure).toContain('ERR_PROXY_CONNECTION_FAILED');
  await context.close();
});

test('the context made after it is not behind that proxy', async ({ browser }) => {
  const context = await browser.newContext();
  const page = await context.newPage();
  expect((await page.goto(LANDED))!.status()).toBe(200);
  await context.close();
});

test('a test that signs in with HTTP credentials', async ({ browser }) => {
  const context = await browser.newContext({ httpCredentials: { username: 'user', password: 'secret' } });
  const page = await context.newPage();
  expect((await page.goto('${origin}/fx/auth?expect=user:secret'))!.status()).toBe(200);
  await context.close();
});

test('the context made after it is not signed in', async ({ browser }) => {
  const context = await browser.newContext();
  const page = await context.newPage();
  const answer = await page.goto('${origin}/fx/auth?expect=user:secret').catch((error) => error);
  // Full Chromium fails the navigation where the headless shell shows the 401.
  if (answer instanceof Error) expect(answer.message).toContain('ERR_INVALID_AUTH_CREDENTIALS');
  else expect(answer!.status()).toBe(401);
  await context.close();
});

test('a test that takes its HTTP credentials away again', async ({ page, context }) => {
  await context.setHTTPCredentials({ username: 'user', password: 'secret' });
  expect((await page.goto('${origin}/fx/auth?expect=user:secret'))!.status()).toBe(200);
  await context.setHTTPCredentials(null);
});

test('the test after it is not signed in either', async ({ page }) => {
  const answer = await page.goto('${origin}/fx/auth?expect=user:secret').catch((error) => error);
  if (answer instanceof Error) expect(answer.message).toContain('ERR_INVALID_AUTH_CREDENTIALS');
  else expect(answer!.status()).toBe(401);
});

test('a test that closes its page', async ({ page, context }) => {
  await page.goto(LANDED);
  await page.evaluate(() => localStorage.setItem('kept', '1'));
  await page.close();
});

test('the test after a closed page starts empty', async ({ page }) => {
  expect(await state(page)).toEqual(fresh);
});

test('a test that leaves a page refusing to unload', async ({ page }) => {
  await page.goto(LANDED);
  await page.evaluate(() => {
    localStorage.setItem('kept', '1');
    addEventListener('beforeunload', (event) => { event.preventDefault(); event.returnValue = 'stay'; });
  });
  await page.mouse.click(10, 10);
});

test('the test after a page refusing to unload starts empty', async ({ page }) => {
  expect(await state(page)).toEqual(fresh);
});
`;

async function targets(origin, reuse) {
  const cwd = await workspace({ 'ferridriver.toml': config(reuse), 'reuse.spec.ts': spec(origin) });
  const result = await run(['test', '--no-inherit', '--headless', '--reporter', 'line'], { cwd });
  passed(result);
  return Object.fromEntries([...result.text.matchAll(/TARGET (\w+) (\S+)/g)].map((m) => [m[1], m[2]]));
}

test('contextReuse hands each test the previous test\'s page, emptied', async () => {
  test.setTimeout(90_000);
  await fixtureServer(async (origin) => {
    const created = await targets(origin, false);
    assert.equal(new Set(Object.values(created)).size, 4, JSON.stringify(created));
    const reused = await targets(origin, true);
    assert.equal(reused.leaver, reused.first, JSON.stringify(reused));
    // The leaver had two pages open; either may be the one kept.
    assert.ok([reused.leaver, reused.popup].includes(reused.next), JSON.stringify(reused));
  });
});
