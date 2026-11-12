// `waitForSelector` over several matches. Playwright takes the first
// (`server/frames.ts::waitForSelector` reads `elements[0]`) unless
// `strict`, where a second match throws; a locator is always strict
// (`client/locator.ts::waitFor` passes `strict: true`).
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

const FIXTURE = dataUrl(
  "<p class='row'>one</p><p class='row'>two</p><p class='gone' style='display:none'>x</p><p class='gone' hidden>y</p>" +
    "<iframe srcdoc='<i class=cell>a</i><i class=cell>b</i>'></iframe>",
);

async function setup(page: Page): Promise<void> {
  await page.goto(FIXTURE);
  await page.locator('p.row').first().waitFor();
}

const message = (work: Promise<unknown>) => work.then(() => 'resolved', (e: Error) => e.message);

test.describe('waitForSelector with several matches', () => {
  test('resolves to the first match', async ({ page }) => {
    await setup(page);
    const row = await page.waitForSelector('.row', { timeout: 3000 });
    expect(await row!.textContent()).toBe('one');
    const attached = await page.waitForSelector('.row', { state: 'attached', timeout: 3000 });
    expect(await attached!.textContent()).toBe('one');
  });

  test('resolves in a frame', async ({ page }) => {
    await setup(page);
    const frame = page.frames().find(f => f !== page.mainFrame())!;
    const cell = await frame.waitForSelector('.cell', { timeout: 3000 });
    expect(await cell!.textContent()).toBe('a');
  });

  test('waits for the first of several to appear', async ({ page }) => {
    await setup(page);
    const waiting = page.waitForSelector('.late', { timeout: 5000 });
    await page.evaluate(() => {
      document.body.insertAdjacentHTML('beforeend', "<b class='late'>first</b><b class='late'>second</b>");
    });
    expect(await (await waiting)!.textContent()).toBe('first');
  });

  test('hidden and detached resolve to null', async ({ page }) => {
    await setup(page);
    expect(await page.waitForSelector('.gone', { state: 'hidden', timeout: 3000 })).toBeNull();
    expect(await page.waitForSelector('.absent', { state: 'detached', timeout: 3000 })).toBeNull();
  });

  test('strict rejects a second match instead of timing out', async ({ page }) => {
    await setup(page);
    const started = Date.now();
    expect(await message(page.waitForSelector('.row', { strict: true, timeout: 10_000 }))).toMatch(
      /strict mode violation.*2 elements/,
    );
    const frame = page.frames().find(f => f !== page.mainFrame())!;
    expect(await message(frame.waitForSelector('.cell', { strict: true, timeout: 10_000 }))).toMatch(
      /strict mode violation.*2 elements/,
    );
    expect(Date.now() - started).toBeLessThan(5000);
    const only = await page.waitForSelector('iframe', { strict: true, timeout: 3000 });
    expect(only).not.toBeNull();
  });

  test('a locator is strict in every state', async ({ page }) => {
    await setup(page);
    const started = Date.now();
    for (const state of ['visible', 'attached', 'hidden', 'detached'] as const) {
      expect(await message(page.locator('.row').waitFor({ state, timeout: 10_000 }))).toMatch(
        /strict mode violation.*2 elements/,
      );
    }
    expect(Date.now() - started).toBeLessThan(5000);
    await page.locator('.row').first().waitFor({ timeout: 3000 });
    await page.locator('.row').nth(1).waitFor({ state: 'attached', timeout: 3000 });
  });
});

// The wait is held in the page and reruns on DOM mutations, which do not
// carry every way an element comes to match. Each case below changes the
// answer with no mutation record in the watched document.
test.describe('waitFor sees changes no mutation record carries', () => {
  test('a rule inserted through the CSSOM', async ({ page }) => {
    await page.setContent("<style>#box { display: none }</style><p id='box'>x</p>");
    await page.evaluate(() => {
      setTimeout(() => document.styleSheets[0].insertRule('#box { display: block !important }', 1), 150);
    });
    await page.locator('#box').waitFor({ state: 'visible', timeout: 3000 });
    await page.evaluate(() => {
      setTimeout(() => document.styleSheets[0].deleteRule(1), 150);
    });
    await page.locator('#box').waitFor({ state: 'hidden', timeout: 3000 });
  });

  test("an element added inside a shadow root", async ({ page }) => {
    await page.setContent("<div id='host'></div>");
    await page.evaluate(() => {
      const root = document.getElementById('host')!.attachShadow({ mode: 'open' });
      setTimeout(() => { root.innerHTML = "<button id='deep'>deep</button>"; }, 150);
    });
    await page.locator('#deep').waitFor({ timeout: 3000 });
    await page.evaluate(() => {
      setTimeout(() => { document.getElementById('host')!.shadowRoot!.innerHTML = ''; }, 150);
    });
    await page.locator('#deep').waitFor({ state: 'detached', timeout: 3000 });
  });

  test('a paused clock neither strands the wait nor its timeout', async ({ page }) => {
    await page.clock.install({ time: 1_000_000_000_000 });
    await page.setContent('<main></main>');
    await page.clock.pauseAt(1_000_000_060_000);
    const started = Date.now();
    expect(await message(page.locator('#never').waitFor({ timeout: 400 }))).toMatch(/Timeout 400ms/);
    expect(Date.now() - started).toBeLessThan(3000);
    await page.evaluate(() => { document.querySelector('main')!.innerHTML = "<i id='now'>now</i>"; });
    await page.locator('#now').waitFor({ timeout: 3000 });
  });

  test('an assertion sees a value set from script', async ({ page }) => {
    await page.setContent("<input id='field'>");
    await page.evaluate(() => {
      setTimeout(() => { (document.getElementById('field') as HTMLInputElement).value = 'set'; }, 150);
    });
    await expect(page.locator('#field')).toHaveValue('set', { timeout: 3000 });
  });
});
