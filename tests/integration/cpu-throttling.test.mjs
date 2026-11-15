import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

// Chromium's throttler suspends the renderer from a thread of its own, on
// wall-clock time. With every core busy (the e2e suite runs a browser per
// worker) that thread is not scheduled in time and the page runs
// unthrottled for stretches: a rate of 6 measured 1.9x on a 4-core Windows
// host inside the suite and 7x on its own. The ratio is only a measurement
// of the throttle in a run of its own, with one worker.
const SPEC = `import { test, expect } from '@ferridriver/test';

test('a CPU throttle of 6 makes the page more than twice as slow', async ({ page, context }) => {
  await page.goto('data:text/html,<body></body>');
  const spin = async () =>
    Number(await page.evaluate(() => {
      const start = performance.now();
      let x = 0;
      for (let i = 0; i < 1e8; i++) x += i % 7;
      return [performance.now() - start, x][0];
    }));
  // The fastest of a few runs is the CPU's own cost; a single run also
  // measures whatever else the machine was doing at that moment. Each run
  // is long enough for the throttler to bite: a 50ms spin on Windows came
  // out under 2x at a rate of 6.
  const fastest = async () => Math.min(await spin(), await spin(), await spin());
  const fast = await fastest();
  await context.setCPUThrottling(6);
  const slow = await fastest();
  await context.setCPUThrottling(null);
  expect(slow).toBeGreaterThan(fast * 2);
});
`;

test('setCPUThrottling slows the page by its rate', async () => {
  test.setTimeout(120_000);
  const cwd = await workspace({
    'ferridriver.toml': `[test]
testMatch = ["*.spec.ts"]
[test.browser]
headless = true
browser = "chromium"
backend = "cdp-pipe"
`,
    'throttle.spec.ts': SPEC,
  });
  passed(await run(['test', '--no-inherit', '--headless', '--workers', '1', '--reporter', 'line'], { cwd }));
});
