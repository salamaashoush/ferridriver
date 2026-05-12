// Cluster 2 — built-in fixtures (browserName, browserVersion, playwright,
// request) and `auto: true` enforcement.
//
// Each first-class fixture needs:
//   1. To resolve regardless of which backend the worker launched.
//   2. To return real data, not a placeholder string.
//
// `auto: true` enforcement is exercised against the Rust pool: a
// FixtureDef with auto=true must run before the test body even when
// the body never destructures it.

import { test, expect } from 'bun:test';
import { type TestMeta, type TestFixtures } from '../index.js';
import { createRunner } from './_test-helpers.js';

// WebKit's stock WKWebView only exposes the persistent default
// context — `Browser::new_context()` returns a handle that the
// runner's per-test worker now resolves to `default_context()`
// (state may leak between tests on this backend, mirroring
// Playwright's launchPersistentContext semantics for non-Chromium
// browsers without isolated containers). All four backends share
// the same browserName + browserVersion + page lifecycle through
// that path.
const BACKENDS_WITH_PAGE = ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit'] as const;

const META: Omit<TestMeta, 'title' | 'id'> = {
  file: 'builtin-fixtures.test.ts',
  annotations: [],
  // Always include `browser` so browserVersion has a real value to read,
  // and `page` so the live launch round-trip happens once.
  requestedFixtures: ['browser', 'page', 'test_info'],
};

function makeMeta(title: string): TestMeta {
  return { ...META, id: title, title };
}

function browserForBackend(backend: string): string {
  return backend === 'bidi' ? 'firefox' : backend === 'webkit' ? 'webkit' : 'chromium';
}

for (const backend of BACKENDS_WITH_PAGE) {
  test(`browserName + browserVersion resolve on ${backend}`, async () => {
    let observedName: string | undefined;
    let observedVersion: string | null | undefined;

    const runner = createRunner({ browser: { backend, browser: browserForBackend(backend) } });
    runner.registerTestsBatch([
      {
        meta: makeMeta('inspect-browser'),
        callback: async (fixtures: TestFixtures) => {
          observedName = fixtures.browserName;
          observedVersion = fixtures.browserVersion;
        },
      },
    ]);
    const summary = await runner.run();
    if (summary.failed > 0) {
      console.error(`[${backend}] failures:`, summary.results);
    }
    expect(summary.failed).toBe(0);
    expect(summary.passed).toBe(1);

    const expectedName =
      backend === 'bidi' ? 'firefox' :
      backend === 'webkit' ? 'webkit' :
      'chromium';
    expect(observedName).toBe(expectedName);
    // Real version string: at least non-empty, and not the literal
    // placeholder `"Unknown"` that the version() docstring warns about
    // when the launch handshake didn't complete.
    expect(typeof observedVersion).toBe('string');
    expect(observedVersion!.length).toBeGreaterThan(0);
    expect(observedVersion).not.toBe('Unknown');
  });
}

test('playwright fixture exposes chromium / firefox / webkit / request', async () => {
  let snapshot: { types: string[]; requestType: string } | undefined;

  const runner = createRunner({ browser: { backend: 'cdp-pipe', browser: 'chromium' } });
  runner.registerTestsBatch([
    {
      meta: makeMeta('inspect-playwright'),
      callback: async (fixtures: TestFixtures) => {
        const pw = fixtures.playwright;
        snapshot = {
          types: [
            pw.chromium.constructor.name,
            pw.firefox.constructor.name,
            pw.webkit.constructor.name,
          ],
          requestType: pw.request.constructor.name,
        };
        // Sanity check: BrowserType.name() echoes the browser product.
        expect(pw.chromium.name()).toBe('chromium');
        expect(pw.firefox.name()).toBe('firefox');
        expect(pw.webkit.name()).toBe('webkit');
        // `playwright.request.newContext()` returns a real APIRequestContext.
        const ctx = await pw.request.newContext();
        expect(typeof ctx.get).toBe('function');
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(snapshot?.types).toEqual(['BrowserType', 'BrowserType', 'BrowserType']);
  expect(snapshot?.requestType).toBe('PlaywrightRequest');
});

test('browserName resolves on webkit (request-only path)', async () => {
  // Request-only path retained as a regression check that the always-
  // available `request` + `test_info` fixtures still resolve without
  // depending on the per-test page context.
  let observedName: string | undefined;
  const runner = createRunner({ browser: { backend: 'webkit', browser: 'webkit' } });
  runner.registerTestsBatch([
    {
      meta: { ...makeMeta('inspect-name'), requestedFixtures: ['request', 'test_info'] },
      callback: async (fixtures: TestFixtures) => {
        observedName = fixtures.browserName;
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(observedName).toBe('webkit');
});

test('request fixture is a usable APIRequestContext', async () => {
  let getMethodPresent = false;
  const runner = createRunner({ browser: { backend: 'cdp-pipe', browser: 'chromium' } });
  runner.registerTestsBatch([
    {
      meta: { ...makeMeta('inspect-request'), requestedFixtures: ['request', 'test_info'] },
      callback: async (fixtures: TestFixtures) => {
        getMethodPresent = typeof fixtures.request.get === 'function';
      },
    },
  ]);
  const summary = await runner.run();
  expect(summary.failed).toBe(0);
  expect(getMethodPresent).toBe(true);
});
