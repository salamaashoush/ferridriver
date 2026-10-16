// page.coverage: JavaScript and CSS coverage. Ported from Playwright's
// tests/library/chromium/js-coverage.spec.ts and css-coverage.spec.ts;
// Playwright offers coverage on Chromium only.
import { test, expect } from '@ferridriver/test';

const PAGE = 'http://127.0.0.1:47831/coverage/simple.html';

test('JavaScript coverage reports which functions ran', async ({ page, browserName }) => {
  if (browserName !== 'chromium') {
    const refused = await page.coverage.startJSCoverage().then(() => 'started', (e: Error) => e.message);
    expect(refused).toContain('Chromium');
    return;
  }
  await page.coverage.startJSCoverage();
  await page.goto(PAGE);
  const entries = await page.coverage.stopJSCoverage();
  const script = entries.find(entry => entry.url.endsWith('/coverage/script.js'))!;
  expect(script.source).toContain('function unused()');
  const count = (name: string) =>
    script.functions.find(fn => fn.functionName === name)!.ranges[0]!.count;
  expect(count('used')).toBe(1);
  expect(count('unused')).toBe(0);
  expect(await page.coverage.stopJSCoverage()).toEqual([]);
});

test('JavaScript coverage forgets earlier documents unless told not to', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Playwright offers coverage on Chromium only');
  await page.coverage.startJSCoverage();
  await page.goto(PAGE);
  await page.goto('http://127.0.0.1:47831/empty.html');
  expect((await page.coverage.stopJSCoverage()).some(entry => entry.url.endsWith('script.js'))).toBe(false);

  await page.coverage.startJSCoverage({ resetOnNavigation: false });
  await page.goto(PAGE);
  await page.goto('http://127.0.0.1:47831/empty.html');
  expect((await page.coverage.stopJSCoverage()).some(entry => entry.url.endsWith('script.js'))).toBe(true);
});

test('CSS coverage reports the rules in use', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Playwright offers coverage on Chromium only');
  await page.coverage.startCSSCoverage();
  await page.goto(PAGE);
  const entries = await page.coverage.stopCSSCoverage();
  const sheet = entries.find(entry => entry.url.endsWith('/coverage/sheet.css'))!;
  expect(sheet.text).toContain('.unused');
  const used = sheet.ranges.map(range => sheet.text!.slice(range.start, range.end));
  expect(used.join('')).toContain('.used { color: red; }');
  expect(used.join('')).not.toContain('.unused');
});

test('starting twice is refused', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Playwright offers coverage on Chromium only');
  await page.coverage.startJSCoverage();
  const again = await page.coverage.startJSCoverage().then(() => 'started', (e: Error) => e.message);
  expect(again).toContain('already enabled');
  await page.coverage.stopJSCoverage();
});
