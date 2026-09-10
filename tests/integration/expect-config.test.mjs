import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

async function suite(config, source) {
  const cwd = await workspace({
    'probe.spec.ts': source,
    'ferridriver.toml': `[test]
      testMatch = ["*.spec.ts"]
      workers = 1
      retries = 0
      timeout = 20000
      outputDir = "out"
      snapshotDir = "snaps"
      reporter = [{ name = "list" }]
      ${config}
      [test.browser]
      browser = "chromium"
      backend = "cdp-pipe"
      headless = true
    `,
  });
  return run(['test', '--no-inherit'], { cwd });
}

const invisible = `import { test, expect } from '@ferridriver/test';
  test('never visible', async ({ page }) => {
    await page.setContent('<div>nothing here</div>');
    await expect(page.locator('#missing')).toBeVisible();
  });`;

for (const [config, timeout] of [
  ['[test.expect]\ntimeout = 900', 900],
  ['expectTimeout = 700', 700],
  ['expectTimeout = 700\n[test.expect]\ntimeout = 1100', 1100],
]) {
  test(`expect configuration reaches the matcher with ${timeout}ms`, async () => {
    const result = await suite(config, invisible);
    assert.equal(result.code, 1, result.text);
    assert.ok(result.text.includes(`Timeout:  ${timeout}ms`), result.text);
    assert.ok(result.elapsed < 15000, `matcher exceeded its configured timeout: ${result.elapsed}ms`);
  });
}

test('each project uses its own expect timeout or the root fallback', async () => {
  const result = await suite(`[test.expect]
    timeout = 2500
    [[test.projects]]
    name = "fast"
    [test.projects.expect]
    timeout = 600
    [[test.projects]]
    name = "slow"`, invisible);
  assert.equal(result.code, 1, result.text);
  for (const timeout of [600, 2500]) assert.ok(result.text.includes(`Timeout:  ${timeout}ms`), result.text);
});

const screenshot = `import { test, expect } from '@ferridriver/test';
  const box = color => '<style>body{margin:0}</style>' +
    '<div id="target" style="width:100px;height:100px;background:#ffffff">' +
    '<div style="width:100px;height:36px;background:' + color + '"></div></div>';
  test('baseline', async ({ page }) => {
    await page.setContent(box('#ffffff'));
    await expect(page.locator('#target')).toHaveScreenshot('delta.png');
  });
  test('within the configured budget', async ({ page }) => {
    await page.setContent(box('#000000'));
    await expect(page.locator('#target')).toHaveScreenshot('delta.png');
  });
  test('a per-call option overrides the configured budget', async ({ page }) => {
    await page.setContent(box('#000000'));
    await expect(page.locator('#target')).toHaveScreenshot('delta.png', { maxDiffPixelRatio: 0.01 });
  });`;

for (const [ratio, failures] of [[0.5, 1], [0.01, 2]]) {
  test(`screenshot comparison uses the ${ratio} config budget and the stricter call override`, async () => {
    const result = await suite(`[test.expect]\ntimeout = 1000\n[test.expect.toHaveScreenshot]\nmaxDiffPixelRatio = ${ratio}`, screenshot);
    assert.equal(result.code, 1, result.text);
    assert.equal(result.text.split('pixels differ').length - 1, failures, result.text);
  });
}

for (const ownBudget of [false, true]) {
  test(`a project replaces the entire expect block, own screenshot budget: ${ownBudget}`, async () => {
    const result = await suite(`[test.expect]
      timeout = 2500
      [test.expect.toHaveScreenshot]
      maxDiffPixelRatio = 0.9
      [[test.projects]]
      name = "narrow"
      [test.projects.expect]
      timeout = 800
      ${ownBudget ? '[test.projects.expect.toHaveScreenshot]\nmaxDiffPixelRatio = 0.9' : ''}`, screenshot);
    assert.equal(result.code, 1, result.text);
    assert.ok(result.text.includes('pixels differ'), result.text);
    if (ownBudget) assert.equal(result.text.split('pixels differ').length - 1, 1, result.text);
  });
}

test('a custom matcher observes the resolved expect timeout', async () => {
  const source = `import { test, expect } from '@ferridriver/test';
    expect.extend({ toSeeTimeout(received) {
      return { pass: this.timeout === received, message: () => 'this.timeout was ' + this.timeout };
    }});
    test('matcher context', async () => { expect(1234).toSeeTimeout(1234); });`;
  passed(await suite('[test.expect]\ntimeout = 1234', source));
  const result = await suite('[test.expect]\ntimeout = 900', source);
  assert.equal(result.code, 1, result.text);
  assert.match(result.text, /this.timeout was 900/);
});
