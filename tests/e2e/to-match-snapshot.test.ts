// `expect(value).toMatchSnapshot()` as Playwright has it: synchronous,
// on a string or a Buffer, comparing image buffers as images.

import { existsSync, readFileSync, rmSync } from 'node:fs';
import { test, describe, expect } from '@ferridriver/test';

const thrown = (body: () => unknown): string => {
  try {
    body();
  } catch (e) {
    return String(e);
  }
  return '';
};

describe('toMatchSnapshot', () => {
  test('a string matches synchronously and a different one throws', async ({}, testInfo) => {
    expect('stable text').toMatchSnapshot('value.txt');
    expect(existsSync(testInfo.snapshotPath('value.txt'))).toBe(true);
    expect('stable text').toMatchSnapshot('value.txt');
    const message = thrown(() => expect('changed text').toMatchSnapshot('value.txt'));
    expect(message).toContain('Snapshot comparison failed');
    expect(message).toContain('+changed text');
  });

  test('.not passes on different content and fails on the same', async () => {
    expect('kept').toMatchSnapshot('kept.txt');
    expect('other').not.toMatchSnapshot('kept.txt');
    expect(thrown(() => expect('kept').not.toMatchSnapshot('kept.txt'))).toContain(
      'Expected result should be different from the actual one.',
    );
    expect(thrown(() => expect('x').not.toMatchSnapshot('never-written.txt'))).toContain(
      'matchers using ".not" won\'t write them automatically.',
    );
  });

  test('a Buffer holding an image is compared as an image within its budget', async ({ page }) => {
    await page.setViewportSize({ width: 120, height: 60 });
    const box = (colour: string) =>
      page.setContent(`<style>html,body{margin:0;background:#fff}</style><div style="width:40px;height:20px;background:${colour}"></div>`);
    await box('#00f');
    expect(await page.screenshot({ scale: 'css' })).toMatchSnapshot('box.png');
    await box('#f00');
    const shot = await page.screenshot({ scale: 'css' });
    expect(thrown(() => expect(shot).toMatchSnapshot('box.png'))).toMatch(
      /800 pixels \(ratio 0\.12 of all image pixels\) are different\./,
    );
    expect(shot).toMatchSnapshot('box.png', { maxDiffPixels: 800 });
    expect(shot).toMatchSnapshot({ name: 'box.png', maxDiffPixelRatio: 0.2 });
  });

  test('path segments name the file and an unnamed Buffer takes its image extension', async ({ page }, testInfo) => {
    expect('nested').toMatchSnapshot(['group', 'nested.txt']);
    expect(existsSync(testInfo.snapshotPath('group', 'nested.txt'))).toBe(true);
    // A flat block rather than text: glyph antialiasing follows the
    // system appearance, and this test is about how the file is named.
    await page.setViewportSize({ width: 120, height: 60 });
    await page.setContent('<style>html,body{margin:0;background:#fff}</style><div style="width:40px;height:20px;background:#0a0"></div>');
    expect(await page.screenshot({ scale: 'css' })).toMatchSnapshot();
    expect(existsSync(testInfo.snapshotPath(`${testInfo.titlePath.slice(1).join('-').replace(/[^\w-]+/g, '-')}-1.png`)))
      .toBe(true);
  });

  test('an unresolved promise is refused with upstream\'s message', async ({ page }) => {
    const pending = page.screenshot();
    expect(thrown(() => expect(pending).toMatchSnapshot('pending.png'))).toContain(
      'An unresolved Promise was passed to toMatchSnapshot(), make sure to resolve it by adding await to it.',
    );
    await pending;
  });
});

describe('toHaveScreenshot baselines', () => {
  test('.not passes on a changed page and fails on the same one', async ({ page }) => {
    await page.setViewportSize({ width: 120, height: 60 });
    await page.setContent('<style>html,body{margin:0;background:#0a0}</style>');
    await expect(page).toHaveScreenshot('not.png');
    await page.setContent('<style>html,body{margin:0;background:#a00}</style>');
    await expect(page).not.toHaveScreenshot('not.png', { timeout: 2000 });
    await page.setContent('<style>html,body{margin:0;background:#0a0}</style>');
    const message = await expect(page)
      .not.toHaveScreenshot('not.png', { timeout: 1000 })
      .then(() => '', e => String(e));
    expect(message).toContain('Expected result should be different from the actual one.');
  });

  test('a name array is a path, and the options bag can carry the name', async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 80, height: 40 });
    await page.setContent('<style>html,body{margin:0;background:#00a}</style>');
    await expect(page).toHaveScreenshot(['shots', 'blue.png']);
    expect(existsSync(testInfo.snapshotPath('shots', 'blue.png', { kind: 'screenshot' }))).toBe(true);
    await expect(page).toHaveScreenshot({ name: 'named.png', maxDiffPixels: 0 });
    expect(existsSync(testInfo.snapshotPath('named.png', { kind: 'screenshot' }))).toBe(true);
  });

  test('a page that never settles cannot become a new baseline', async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 80, height: 40 });
    rmSync(testInfo.snapshotPath('unstable.png', { kind: 'screenshot' }), { force: true });
    // A different width on every frame: no two captures can agree.
    await page.setContent(
      '<style>html,body{margin:0}</style><div id=b style="height:40px;background:#00f"></div>' +
        '<script>let n = 0; const tick = () => { document.getElementById("b").style.width = (n++ % 80) + "px"; requestAnimationFrame(tick); }; tick();</script>',
    );
    const message = await expect(page)
      .toHaveScreenshot('unstable.png', { timeout: 1500 })
      .then(() => '', e => String(e));
    expect(message).toContain('Failed to take two consecutive stable screenshots.');
    expect(existsSync(testInfo.snapshotPath('unstable.png', { kind: 'screenshot' }))).toBe(false);
  });
});

describe('toMatchAriaSnapshot files', () => {
  test('a missing file is generated from the live tree, then matched', async ({ page }, testInfo) => {
    // The aria path template has no project in it, so every project would
    // share (and delete) one file.
    const name = `nav-${testInfo.project.name}.aria.yml`;
    const file = testInfo.snapshotPath(name, { kind: 'aria' });
    rmSync(file, { force: true });
    await page.setContent('<nav><a href="/a">Home</a><button>Save</button></nav>');
    await expect(page.locator('nav')).toMatchAriaSnapshot({ name, timeout: 500 });
    const written = readFileSync(file, 'utf8');
    expect(written).toContain('- link "Home"');
    expect(written).toContain('- button "Save"');
    await expect(page.locator('nav')).toMatchAriaSnapshot({ name });
    await page.setContent('<nav><a href="/a">Home</a><button>Discard</button></nav>');
    const message = await expect(page.locator('nav'))
      .toMatchAriaSnapshot({ name, timeout: 500 })
      .then(() => '', e => String(e));
    expect(message).toContain('Discard');
    await expect(page.locator('nav')).not.toMatchAriaSnapshot({ name, timeout: 500 });
  });

  test('.not cannot generate a baseline', async ({ page }) => {
    await page.setContent('<p>text</p>');
    const message = await expect(page)
      .not.toMatchAriaSnapshot({ name: 'never.aria.yml' })
      .then(() => '', e => String(e));
    expect(message).toContain('Matchers using ".not" can\'t generate new baselines');
  });
});
