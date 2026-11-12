import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

const PROJECTS = [
  ['cdp-pipe', 'chromium', 'cdp-pipe'],
  ['cdp-raw', 'chromium', 'cdp-raw'],
  ['bidi', 'firefox', 'bidi'],
  ['webkit', 'webkit', 'webkit'],
];

const config = (stableCheck) => `[test]
testMatch = ["*.spec.ts"]
${stableCheck ? `stableCheck = "${stableCheck}"` : ''}
[test.browser]
headless = true
${PROJECTS.map(([name, browser, backend]) => `
[[test.projects]]
name = "${name}"
[test.projects.browser]
browser = "${browser}"
backend = "${backend}"
headless = true
`).join('')}`;

// A button that reports, on every click, whether it was still being moved.
const SPEC = `import { test, expect } from '@ferridriver/test';
const HTML = \`<style>#b{position:absolute;left:0;top:40px;width:80px;height:30px}</style>
<button id="b">go</button>
<script>
  const b = document.getElementById('b');
  window.moving = false;
  window.clicks = [];
  window.frames = 0;
  b.addEventListener('click', () => window.clicks.push(window.moving ? 'moving' : 'still'));
  window.byTransition = (ms) => {
    window.moving = true;
    b.style.transition = 'left ' + ms + 'ms linear';
    b.addEventListener('transitionend', () => { window.moving = false; }, { once: true });
    b.style.left = '200px';
  };
  window.byFrames = (count) => {
    window.moving = true;
    let n = 0;
    const step = () => {
      b.style.left = (++n * 3) + 'px';
      if (n < count) requestAnimationFrame(step); else window.moving = false;
    };
    requestAnimationFrame(step);
  };
  window.byTimer = (count, every) => {
    window.moving = true;
    let n = 0;
    const id = setInterval(() => {
      b.style.left = (++n * 3) + 'px';
      if (n >= count) { clearInterval(id); window.moving = false; }
    }, every);
  };
  window.countFrames = () => { const tick = () => { window.frames++; requestAnimationFrame(tick); }; requestAnimationFrame(tick); };
</script>\`;
const clicks = (page) => page.evaluate(() => (window as any).clicks);

test('a click waits out a CSS transition', async ({ page }) => {
  await page.setContent(HTML);
  await page.evaluate(() => (window as any).byTransition(300));
  await page.locator('#b').click();
  expect(await clicks(page)).toEqual(['still']);
});

test('a click waits out a frame-driven move', async ({ page }) => {
  await page.setContent(HTML);
  await page.evaluate(() => (window as any).byFrames(15));
  await page.locator('#b').click();
  expect(await clicks(page)).toEqual(['still']);
});

test('a click waits out a timer-driven move', async ({ page }) => {
  await page.setContent(HTML);
  await page.evaluate(() => (window as any).byTimer(60, 4));
  await page.locator('#b').click();
  expect(await clicks(page)).toEqual(['still']);
});

test('a hover waits out a CSS transition', async ({ page }) => {
  await page.setContent(HTML);
  await page.evaluate(() => {
    const b = document.getElementById('b')!;
    b.addEventListener('mouseenter', () => (window as any).clicks.push((window as any).moving ? 'moving' : 'still'));
    (window as any).byTransition(300);
  });
  await page.locator('#b').hover();
  expect(await clicks(page)).toEqual(['still']);
});

// The fewest frames any two consecutive clicks were apart. Playwright's
// check needs two frame callbacks of its own between them, so that is its
// floor however the browser paces frames; one frame is the other's.
test('frames between clicks of an element at rest', async ({ page }, testInfo) => {
  await page.setContent(HTML);
  await page.locator('#b').click();
  await page.evaluate(() => {
    (window as any).at = [];
    document.getElementById('b')!.addEventListener('click', () => (window as any).at.push((window as any).frames));
    (window as any).countFrames();
  });
  for (let i = 0; i < 30; i++) await page.locator('#b').click();
  const at: number[] = await page.evaluate(() => (window as any).at);
  expect(at.length).toBe(30);
  const apart = Math.min(...at.slice(1).map((frame, i) => frame - at[i]));
  console.log('FRAMES ' + testInfo.project.name + ' ' + apart);
});
`;

// One run per project: all four browsers one after another in a single run
// outlast the 25s the `probe` command allows on a CI runner.
async function frames(stableCheck) {
  const cwd = await workspace({ 'ferridriver.toml': config(stableCheck), 'stable.spec.ts': SPEC });
  const spent = new Map();
  for (const [name] of PROJECTS) {
    const result = await run(
      ['test', '--no-inherit', '--headless', '--workers', '1', '--reporter', 'line', '--project', name],
      { cwd },
    );
    passed(result);
    const reported = [...result.text.matchAll(/FRAMES (\S+) (\d+)/g)];
    assert.deepEqual(reported.map((m) => m[1]), [name], result.text);
    spent.set(name, Number(reported[0][2]));
  }
  return spent;
}

test('stableCheck = "frame" holds a click for every kind of mover and needs fewer frames', async () => {
  test.setTimeout(300_000);
  const playwright = await frames(undefined);
  const frame = await frames('frame');
  for (const [name] of PROJECTS) {
    assert.ok(playwright.get(name) >= 2, `${name}: the default check let two clicks land ${playwright.get(name)} frames apart`);
    assert.ok(
      frame.get(name) < playwright.get(name),
      `${name}: clicks were never closer than ${frame.get(name)} frames under "frame", ${playwright.get(name)} under the default`,
    );
  }
});

test('stableCheck rejects a value it does not know', async () => {
  const cwd = await workspace({ 'ferridriver.toml': config('never'), 'stable.spec.ts': SPEC });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /stableCheck|unknown variant/);
});
