// Ported from crates/ferridriver-cli/tests/backends_support/
// browser_type.rs — per-method BrowserType factory probes
// (types.d.ts:15046). Each test spins up SECONDARY browsers that live
// for the duration of a single test. Test titles mirror the original
// Rust fn names.

import { test, describe, expect, safari as safariFactory } from '@ferridriver/test';
import { runOnceAcrossProjects } from './helpers/projects';

describe('browser type', () => {
  test('browser_type_name', async () => {
    runOnceAcrossProjects();
    // The factories exist regardless of which backend the current
    // project drives — Playwright likewise exposes all three.
    expect(chromium().name()).toBe('chromium');
    expect(firefox().name()).toBe('firefox');
    expect(webkit().name()).toBe('webkit');
    expect(safari().name()).toBe('safari');
    expect(safariFactory().name()).toBe('safari');
  });

  test('browser_type_executable_path', async () => {
    runOnceAcrossProjects();
    const path = chromium().executablePath();
    expect(typeof path).toBe('string');
    expect(path!.length).toBeGreaterThan(0);
  });

  test('browser_type_chromium_launch', async () => {
    runOnceAcrossProjects();
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
    runOnceAcrossProjects();
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
    runOnceAcrossProjects();
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

  test('browser_contexts_lists_only_live_contexts', async ({ browserName }) => {
    // `browser.contexts()` must reflect what is currently open, not
    // everything ever opened. The registry behind it was append-only, so
    // a closed context stayed listed for the life of the process — a
    // wrong answer, and an unbounded allocation in any process that
    // opens a context per session.
    //
    // Uses its own browser, like every other test in this file: creating
    // and closing contexts on the shared worker browser perturbs the
    // suite (it drives extra speculative preconnections at the fixture
    // server, which is enough to fail a concurrent history-traversal
    // navigation).
    test.slow();
    const factory = browserName === 'firefox' ? firefox() : browserName === 'webkit' ? webkit() : chromium();
    const b = await factory.launch({ headless: true });
    try {
      const before = (await b.contexts()).length;
      const a = await b.newContext();
      const c = await b.newContext();
      // Contexts materialize lazily, so open a page in one to prove the
      // listing tracks a context that really exists in the browser.
      await a.newPage();
      expect((await b.contexts()).length).toBe(before + 2);
      await a.close();
      expect((await b.contexts()).length).toBe(before + 1);
      await c.close();
      expect((await b.contexts()).length).toBe(before);
    } finally {
      await b.close();
    }
  });
});
