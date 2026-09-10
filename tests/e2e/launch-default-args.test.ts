// `ignoreDefaultArgs` — which of the bundled switches a launch drops.
//
// Playwright's `boolean | string[]`. Chromium-only, because it is the
// only launch path that injects a switch list at all; Firefox and
// WebKit get the transport flags and nothing else, and answer with the
// typed Unsupported rather than pretend to filter an empty list.
//
// The assertions read the browser's OWN command line back over CDP
// rather than inferring the effect from behaviour. A switch that is
// gone is gone, and nothing has to be reasoned about to see it.

import { test, describe, expect } from '@ferridriver/test';

/** What Chrome says it was started with. */
async function argv(options: Record<string, unknown>): Promise<string[] | 'refused'> {
  const browser = await chromium().launch({ headless: true, ...options });
  try {
    const session = await browser.newBrowserCDPSession();
    try {
      const line = (await session.send('Browser.getBrowserCommandLine', {})) as { arguments?: string[] };
      return line.arguments ?? [];
    } catch {
      // Chrome only reports its command line to an automated browser,
      // and `--enable-automation` is itself one of the defaults.
      return 'refused';
    } finally {
      await session.detach();
    }
  } finally {
    await browser.close();
  }
}

describe('ignoreDefaultArgs', () => {
  test('a named switch is dropped and the rest are kept', async ({ browserName }) => {
    if (browserName !== 'chromium') {
      // `chromium()` is always Chromium, so there is nothing
      // project-specific here beyond not running it four times.
      return;
    }

    const all = await argv({});
    expect(Array.isArray(all)).toBe(true);
    const before = all as string[];
    expect(before.includes('--disable-popup-blocking')).toBe(true);
    expect(before.includes('--disable-hang-monitor')).toBe(true);

    const after = (await argv({ ignoreDefaultArgs: ['--disable-popup-blocking'] })) as string[];
    expect(after.includes('--disable-popup-blocking')).toBe(false);
    // Only that one: a filter that dropped the list is not a filter.
    expect(after.includes('--disable-hang-monitor')).toBe(true);
    expect(after.length).toBe(before.length - 1);

    // Naming a switch that is not there drops nothing.
    const untouched = (await argv({ ignoreDefaultArgs: ['--no-such-switch'] })) as string[];
    expect(untouched.length).toBe(before.length);
  });

  test('true drops the lot, and the browser still starts', async ({ browserName }) => {
    if (browserName !== 'chromium') {
      return;
    }
    // `--enable-automation` is one of the defaults, so a browser
    // launched without them will not report its command line at all.
    // The refusal IS the evidence that they went.
    // ignoreDefaultArgs also removes the flag from headless: true.
    const options = { headless: true, ignoreDefaultArgs: true, args: ['--headless', '--no-first-run'] };
    expect(await argv(options)).toBe('refused');

    const bare = await chromium().launch(options);
    try {
      const page = await bare.newPage();
      await page.setContent('<html><body><main id="bare">up</main></body></html>');
      expect(await page.textContent('#bare')).toBe('up');
    } finally {
      await bare.close();
    }
  });

  test('the backends with no switch list to drop say so', async ({ browserName }) => {
    if (browserName !== 'chromium') {
      return;
    }
    // Named for the backend rather than the product: `firefox()` is the
    // BiDi launch path, and the error says which one refused. Neither
    // injects a switch list, so filtering one is a request there is no
    // honest answer to.
    for (const [name, factory] of [
      ['bidi', firefox],
      ['webkit', webkit],
    ] as const) {
      let message = '';
      try {
        const browser = await factory().launch({ ignoreDefaultArgs: true });
        await browser.close();
      } catch (e) {
        message = String(e);
      }
      expect(message.includes('ignoreDefaultArgs')).toBe(true);
      expect(message.includes(name)).toBe(true);
    }
  });
});
