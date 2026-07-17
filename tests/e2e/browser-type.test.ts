// Ported from crates/ferridriver-cli/tests/backends_support/
// browser_type.rs — per-method BrowserType factory probes
// (types.d.ts:15046). Each test spins up SECONDARY browsers that live
// for the duration of a single test. Test titles mirror the original
// Rust fn names.

import { test, describe, expect } from '@ferridriver/test';

describe('browser type', () => {
  test('browser_type_name', async () => {
    // The factories exist regardless of which backend the current
    // project drives — Playwright likewise exposes all three.
    expect(chromium().name()).toBe('chromium');
    expect(firefox().name()).toBe('firefox');
    expect(webkit().name()).toBe('webkit');
  });

  test('browser_type_executable_path', async () => {
    const path = chromium().executablePath();
    expect(typeof path).toBe('string');
    expect(path!.length).toBeGreaterThan(0);
  });

  test('browser_type_chromium_launch', async () => {
    // Drives BrowserType -> Browser -> handshake plumbing end-to-end;
    // the handshake captures a real product string.
    test.slow();
    const browser = await chromium().launch({ headless: true });
    try {
      const version = String(await browser.version());
      expect(version.includes('Chrome') || version.includes('Chromium') || version.includes('Headless')).toBe(true);
    } finally {
      await browser.close();
    }
  });

  test('browser_type_chromium_transport_ws', async () => {
    // The transport override actually selects the WebSocket backend.
    test.slow();
    const browser = await chromium({ transport: 'ws' }).launch({ headless: true });
    try {
      const version = String(await browser.version());
      expect(version.includes('Chrome') || version.includes('Chromium') || version.includes('Headless')).toBe(true);
    } finally {
      await browser.close();
    }
  });

  test('browser_type_connect_over_cdp_chromium_only', async () => {
    // connectOverCDP is a real protocol-level Chromium constraint — the
    // rejection is typed, not a stub.
    let firefoxErr = '';
    try {
      await firefox().connectOverCDP('ws://127.0.0.1:65535');
    } catch (e) {
      firefoxErr = String((e as Error).message ?? e);
    }
    let webkitErr = '';
    try {
      await webkit().connectOverCDP('ws://127.0.0.1:65535');
    } catch (e) {
      webkitErr = String((e as Error).message ?? e);
    }
    expect(firefoxErr.includes('Chromium') || firefoxErr.includes('connectOverCDP')).toBe(true);
    expect(webkitErr.includes('Chromium') || webkitErr.includes('connectOverCDP')).toBe(true);
  });

  test('browser_type_launch_persistent_context', async () => {
    // Launching with a userDataDir populates the profile; a second
    // launch against the SAME dir must succeed (proves the first
    // browser shut down, releasing the SingletonLock) and produce a
    // usable page attached to the existing profile.
    test.slow();
    const dir = test.info().outputPath('persistent-profile');
    {
      const ctx = await chromium().launchPersistentContext(dir, { headless: true });
      await ctx.newPage();
      await ctx.close();
    }
    const ctx = await chromium().launchPersistentContext(dir, { headless: true });
    try {
      const p = await ctx.newPage();
      const ua = (await p.evaluate(() => navigator.userAgent)) as string;
      expect(ua.includes('Chrome') || ua.includes('Chromium') || ua.includes('HeadlessChrome')).toBe(true);
    } finally {
      await ctx.close();
    }
  });
});
