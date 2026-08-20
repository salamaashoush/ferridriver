// The standing parity gate: the Playwright surfaces a suite reaches for
// before it writes a line of its own framework code. Run with NO
// extensions loaded — everything here is core, and stays core.

import { test as base, describe, expect, mergeTests, devices } from '@ferridriver/test';

// ── mergeTests over two independent extend chains ──────────────────

const withUser = base.extend<{ user: string }>({
  user: async ({}, use) => {
    await use('ada');
  },
});

const withHost = base.extend<{ host: string }>({
  host: async ({}, use) => {
    await use('example.test');
  },
});

const test = mergeTests(withUser, withHost);

// ── expect.extend ───────────────────────────────────────────────────

expect.extend({
  toBeTheAnswer(received: number) {
    return {
      pass: received === 42,
      message: () => `expected ${received} to be 42`,
    };
  },
});

describe('parity acceptance', () => {
  test('mergeTests composes both chains into one test', async ({ user, host }) => {
    expect(user).toBe('ada');
    expect(host).toBe('example.test');
  });

  test('a custom matcher runs, and negates', async ({}) => {
    (expect(42) as any).toBeTheAnswer();
    (expect(41) as any).not.toBeTheAnswer();
  });

  test('the config reached the run', async ({}, testInfo) => {
    // `forbidOnly` came from the SECOND defineConfig argument, which is
    // what proves the fold puts the rightmost on top.
    expect(testInfo.config.forbidOnly).toBe(true);
    // Both projects are declared, and this run is one of them.
    expect(['desktop', 'mobile'].includes(testInfo.project.name)).toBe(true);
  });

  test('the device descriptor reached the browser', async ({ page }, testInfo) => {
    const mobile = testInfo.project.name === 'mobile';
    const expected = mobile ? devices['iPhone 15'] : devices['Desktop Chrome'];

    await page.setContent('<meta name="viewport" content="width=device-width"><body>device</body>');
    const seen = (await page.evaluate(() => ({
      agent: navigator.userAgent,
      width: window.innerWidth,
      touch: 'ontouchstart' in window || navigator.maxTouchPoints > 0,
    }))) as { agent: string; width: number; touch: boolean };

    expect(seen.agent).toBe(expected.userAgent);
    expect(seen.width).toBe(expected.viewport.width);
    expect(seen.touch).toBe(expected.hasTouch);
  });

  test('snapshotPathTemplate names the baseline', async ({ page }, testInfo) => {
    const declared = testInfo.snapshotPath('shot.png', { kind: 'screenshot' });
    // {projectName} and {platform} both resolved — the two placeholders
    // that keep baselines from colliding across a matrix.
    expect(declared.includes(`/${testInfo.project.name}/`)).toBe(true);
    expect(declared.endsWith('shot.png')).toBe(true);

    await page.setContent('<div style="width:40px;height:40px;background:#123456"></div>');
    await expect(page.locator('div')).toHaveScreenshot('shot.png');
    expect(fs.existsSync(declared)).toBe(true);
  });
});
