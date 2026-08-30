// Ported from crates/ferridriver-cli/tests/backends_support/
// browser_context_options.rs — per-option BrowserContextOptions probes
// (types.d.ts:22229). Each test creates a FRESH context, applies a
// single option, opens a page, and asserts a page-visible side effect
// produced ONLY when the option took effect. Test titles mirror the
// original Rust fn names. Real-origin needs go through the fixture
// server (baseURL); the observable proxy uses /fx/proxy-info +
// /fx/proxy-log.

import { test, describe, expect } from '@ferridriver/test';
import type { Browser, BrowserContextOptions } from '@ferridriver/test';
import { fxProxyLog, fxProxyLogReset, fxProxyUrl } from './helpers/server';

// Firefox/BiDi rejects unsupported context options with a typed error
// naming the option at newPage (attach) time — assert that contract
// instead of silently skipping.
async function expectOptionUnsupportedOnFirefox(
  browser: Browser,
  options: BrowserContextOptions,
  label: string,
): Promise<void> {
  const ctx = await browser.newContext(options);
  try {
    let msg = '';
    try {
      await ctx.newPage();
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect([label, msg.includes(label)]).toEqual([label, true]);
  } finally {
    await ctx.close();
  }
}

describe('context options', () => {
  test('context_options_user_agent', async ({ browser }) => {
    const ctx = await browser.newContext({ userAgent: 'FerriUA/1.0 (RuleNine)' });
    try {
      const p = await ctx.newPage();
      const ua = (await p.evaluate(() => navigator.userAgent)) as string;
      expect(ua.includes('FerriUA/1.0 (RuleNine)')).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_locale', async ({ browser }) => {
    const ctx = await browser.newContext({ locale: 'de-DE' });
    try {
      const p = await ctx.newPage();
      const lang = (await p.evaluate(() => navigator.language)) as string;
      expect(lang.startsWith('de')).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_timezone', async ({ browser }) => {
    const ctx = await browser.newContext({ timezoneId: 'America/New_York' });
    try {
      const p = await ctx.newPage();
      const tz = await p.evaluate(() => Intl.DateTimeFormat().resolvedOptions().timeZone);
      expect(tz).toBe('America/New_York');
    } finally {
      await ctx.close();
    }
  });

  test('context_options_color_scheme', async ({ browser, browserName }) => {
    if (browserName === 'firefox') {
      // Firefox's BiDi has no working colorScheme emulation command
      // (Playwright's BiDi updateEmulateMedia is an empty stub).
      await expectOptionUnsupportedOnFirefox(browser, { colorScheme: 'dark' }, 'colorScheme');
      return;
    }
    const ctx = await browser.newContext({ colorScheme: 'dark' });
    try {
      const p = await ctx.newPage();
      expect(await p.evaluate(() => matchMedia('(prefers-color-scheme: dark)').matches)).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_reduced_motion', async ({ browser, browserName }) => {
    if (browserName === 'firefox') {
      await expectOptionUnsupportedOnFirefox(browser, { reducedMotion: 'reduce' }, 'reducedMotion');
      return;
    }
    const ctx = await browser.newContext({ reducedMotion: 'reduce' });
    try {
      const p = await ctx.newPage();
      expect(await p.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches)).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_forced_colors', async ({ browser, browserName }) => {
    if (browserName === 'firefox') {
      await expectOptionUnsupportedOnFirefox(browser, { forcedColors: 'active' }, 'forcedColors');
      return;
    }
    const ctx = await browser.newContext({ forcedColors: 'active' });
    try {
      const p = await ctx.newPage();
      expect(await p.evaluate(() => matchMedia('(forced-colors: active)').matches)).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_viewport', async ({ browser }) => {
    const ctx = await browser.newContext({ viewport: { width: 800, height: 600 } });
    try {
      const p = await ctx.newPage();
      const dims = (await p.evaluate(() => ({ w: window.innerWidth, h: window.innerHeight }))) as {
        w: number;
        h: number;
      };
      expect(dims.w).toBe(800);
      expect(dims.h).toBe(600);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_javascript_enabled', async ({ browser, browserName }) => {
    if (browserName === 'firefox') {
      // The backend sends emulation.setScriptingEnabled; shipping
      // Firefox rejects it as unknown command — the protocol error
      // surfaces (naming the option) instead of a silent no-op.
      await expectOptionUnsupportedOnFirefox(browser, { javaScriptEnabled: false }, 'javaScriptEnabled');
      return;
    }
    // With JS disabled the inline script cannot set the dataset
    // attribute; evaluate still works (the devtools channel is separate
    // from page-script execution).
    const ctx = await browser.newContext({ javaScriptEnabled: false });
    try {
      const p = await ctx.newPage();
      await p.goto("data:text/html,<body><script>document.body.dataset.set='yes'</script></body>");
      const html = (await p.evaluate(() => document.body.outerHTML)) as string;
      expect(html.includes('data-set')).toBe(false);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_geolocation', async ({ browser, baseURL }) => {
    // Geolocation needs a secure context — data:/about: are opaque
    // origins, http://127.0.0.1 is secure in both engines.
    const ctx = await browser.newContext({
      geolocation: { latitude: 12.5, longitude: 34.75, accuracy: 1 },
      permissions: ['geolocation'],
      baseURL,
    });
    try {
      const p = await ctx.newPage();
      await p.goto('/fx/landed');
      const coords = (await p.evaluate(
        () =>
          new Promise((resolve) => {
            if (!navigator.geolocation) {
              resolve({ error: 'no geolocation api' });
              return;
            }
            navigator.geolocation.getCurrentPosition(
              (pos) => resolve({ lat: pos.coords.latitude, lng: pos.coords.longitude }),
              (err) => resolve({ error: err.code + ':' + err.message }),
              { timeout: 4000 },
            );
          }),
      )) as { lat?: number; lng?: number; error?: string };
      expect(coords.error).toBeUndefined();
      expect(Math.abs((coords.lat ?? 0) - 12.5)).toBeLessThan(0.5);
      expect(Math.abs((coords.lng ?? 0) - 34.75)).toBeLessThan(0.5);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_extra_http_headers', async ({ browser, baseURL }) => {
    // The override header rides on every request the context issues —
    // observed via the fixture server's /fx/echo-headers JSON echo.
    const ctx = await browser.newContext({ extraHTTPHeaders: { 'x-rule-nine': 'pingpong' } });
    try {
      const p = await ctx.newPage();
      await p.goto(`${baseURL}/fx/echo-headers`);
      const body = (await p.evaluate(() => document.body.textContent)) as string;
      const headers = JSON.parse(body) as Record<string, string>;
      expect(headers['x-rule-nine']).toBe('pingpong');
    } finally {
      await ctx.close();
    }
  });

  test('context_options_offline', async ({ browser }) => {
    const ctx = await browser.newContext({ offline: true });
    try {
      const p = await ctx.newPage();
      await p.goto('data:text/html,<body>offline-test</body>');
      const result = (await p.evaluate(async () => {
        try {
          await fetch('http://127.0.0.1:1/never');
          return { ok: true };
        } catch (e) {
          return { ok: false, msg: String((e as Error).message ?? e) };
        }
      })) as { ok: boolean };
      expect(result.ok).toBe(false);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_device_scale_factor', async ({ browser }) => {
    const ctx = await browser.newContext({ viewport: { width: 800, height: 600 }, deviceScaleFactor: 2 });
    try {
      const p = await ctx.newPage();
      const dpr = (await p.evaluate(() => window.devicePixelRatio)) as number;
      expect(Math.abs(dpr - 2)).toBeLessThan(0.01);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_has_touch', async ({ browser, browserName }) => {
    if (browserName === 'firefox') {
      // browsingContext.setViewport has no touch field and Firefox
      // exposes no other command for it — typed rejection, not a
      // silent drop.
      await expectOptionUnsupportedOnFirefox(
        browser,
        { viewport: { width: 800, height: 600 }, hasTouch: true },
        'hasTouch',
      );
      return;
    }
    const ctx = await browser.newContext({ viewport: { width: 800, height: 600 }, hasTouch: true });
    try {
      const p = await ctx.newPage();
      expect(await p.evaluate(() => 'ontouchstart' in window || navigator.maxTouchPoints > 0)).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_set_http_credentials', async ({ browser, baseURL }) => {
    // With credentials set the backend answers the /fx/auth Basic
    // challenge (user:pass) -> 200 AUTHED; this only happens when the
    // credentials took effect. CDP answers via Fetch.authRequired,
    // WebKit via Emulation.setAuthCredentials (wkPage parity), BiDi
    // via the authRequired-phase intercept + network.continueWithAuth.
    const ctx = await browser.newContext({});
    try {
      const p = await ctx.newPage();
      await ctx.setHTTPCredentials({ username: 'user', password: 'pass' });
      const r = await p.goto(`${baseURL}/fx/auth`);
      expect(r?.status()).toBe(200);
      const body = (await p.evaluate(() => document.body.textContent)) as string;
      expect(body.includes('AUTHED')).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_pages_lists_open_tabs', async ({ browser, baseURL }) => {
    // context.pages() returns every open page in creation order with
    // live Page handles (url() reflects each tab's document).
    const ctx = await browser.newContext({});
    try {
      const p1 = await ctx.newPage();
      await p1.goto(`${baseURL}/fx/landed`);
      expect((await ctx.pages()).length).toBe(1);
      const p2 = await ctx.newPage();
      const pages = await ctx.pages();
      expect(pages.length).toBe(2);
      const urls = pages.map((p) => p.url());
      expect(urls.some((u) => u.includes('/fx/landed'))).toBe(true);
      await p2.close();
      expect((await ctx.pages()).length).toBe(1);
    } finally {
      await ctx.close();
    }
  });

  test('context_set_default_timeout', async ({ browser }) => {
    const ctx = await browser.newContext({});
    try {
      ctx.setDefaultTimeout(50);
      const p = await ctx.newPage();
      await p.goto('data:text/html,<body>timeout-probe</body>');
      // Set AFTER the navigation: what this asserts is the selector
      // timeout, and a 50ms budget over a real navigation is a race the
      // test loses under load rather than a behaviour worth pinning.
      ctx.setDefaultNavigationTimeout(50);
      let err = '';
      try {
        await p.waitForSelector('#never-ever', { timeout: 50 });
      } catch (e) {
        err = String((e as Error).message ?? e);
      }
      expect(err.toLowerCase().includes('timeout') || err.toLowerCase().includes('timed out')).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_is_closed_and_browser', async ({ browser }) => {
    const ctx = await browser.newContext({});
    expect(await ctx.isClosed()).toBe(false);
    expect(ctx.browser() != null).toBe(true);
    const ver = String(await ctx.browser()!.version());
    expect(ver.length).toBeGreaterThan(0);
    await ctx.close();
    expect(await ctx.isClosed()).toBe(true);
  });

  test('context_route_and_unroute', async ({ browser }) => {
    const ctx = await browser.newContext({});
    try {
      const p = await ctx.newPage();
      const matcher = 'https://ferri.test/**';
      await ctx.route(matcher, (route) => {
        route.fulfill({ status: 200, contentType: 'text/html', body: '<body>ROUTED</body>' });
      });
      await p.goto('https://ferri.test/page');
      const routed = (await p.evaluate(() => document.body.textContent)) as string;
      await ctx.unroute(matcher);
      expect(routed.includes('ROUTED')).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_route_times_shared_across_pages', async ({ browser }) => {
    // A times:1 context route claimed by page A is exhausted for page B
    // too, and newer registrations win (newest-first within scope).
    const ctx = await browser.newContext({});
    try {
      const matcher = 'https://ferri.test/**';
      await ctx.route(matcher, (route) => {
        route.fulfill({ status: 200, contentType: 'text/html', body: '<body>BASE</body>' });
      });
      await ctx.route(
        matcher,
        (route) => {
          route.fulfill({ status: 200, contentType: 'text/html', body: '<body>LIMITED</body>' });
        },
        { times: 1 },
      );
      const p1 = await ctx.newPage();
      const p2 = await ctx.newPage();
      await p1.goto('https://ferri.test/one');
      const first = (await p1.evaluate(() => document.body.textContent)) as string;
      await p2.goto('https://ferri.test/two');
      const second = (await p2.evaluate(() => document.body.textContent)) as string;
      expect(first.includes('LIMITED')).toBe(true);
      expect(second.includes('BASE')).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_route_applies_to_future_page', async ({ browser }) => {
    const ctx = await browser.newContext({});
    try {
      await ctx.route('https://ferri.test/**', (route) => {
        route.fulfill({ status: 200, contentType: 'text/html', body: '<body>FUTURE</body>' });
      });
      const p = await ctx.newPage();
      await p.goto('https://ferri.test/later');
      const routed = (await p.evaluate(() => document.body.textContent)) as string;
      expect(routed.includes('FUTURE')).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('route_scope_precedence_and_unroute_all', async ({ browser }) => {
    // Page routes beat context routes regardless of registration order;
    // page.unrouteAll leaves context routes active; context.unrouteAll
    // removes them (the fake host then fails to resolve).
    const ctx = await browser.newContext({});
    try {
      const matcher = 'https://ferri.test/**';
      const p = await ctx.newPage();
      await p.route(matcher, (route) => {
        route.fulfill({ status: 200, contentType: 'text/html', body: '<body>PAGE</body>' });
      });
      await ctx.route(matcher, (route) => {
        route.fulfill({ status: 200, contentType: 'text/html', body: '<body>CTX</body>' });
      });
      await p.goto('https://ferri.test/a');
      const withBoth = (await p.evaluate(() => document.body.textContent)) as string;
      await p.unrouteAll();
      await p.goto('https://ferri.test/b');
      const afterPageClear = (await p.evaluate(() => document.body.textContent)) as string;
      await ctx.unrouteAll();
      let contextCleared = false;
      try {
        await p.goto('https://ferri.test/c', { timeout: 3000 });
      } catch {
        contextCleared = true;
      }
      expect(withBoth.includes('PAGE')).toBe(true);
      expect(afterPageClear.includes('CTX')).toBe(true);
      expect(contextCleared).toBe(true);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_service_workers_block', async ({ browser }) => {
    const ctx = await browser.newContext({ serviceWorkers: 'block' });
    try {
      const p = await ctx.newPage();
      await p.goto('data:text/html,<body></body>');
      const result = (await p.evaluate(async () => {
        if (!navigator.serviceWorker) return { hasSW: false, rejected: false };
        try {
          await navigator.serviceWorker.register('/sw.js');
          return { hasSW: true, rejected: false };
        } catch {
          return { hasSW: true, rejected: true };
        }
      })) as { hasSW: boolean; rejected: boolean };
      // data: URLs may not expose navigator.serviceWorker at all —
      // vacuous pass; when present, the block must force a rejection.
      if (result.hasSW) {
        expect(result.rejected).toBe(true);
      }
    } finally {
      await ctx.close();
    }
  });

  // `recordHar` was accepted on the bag and dropped on the floor: both
  // bindings hard-coded `record_har: None`, so no archive was ever
  // written and nothing said so. The recorder itself already existed —
  // `tracing.startHar` and `routeFromHAR(update: true)` share it — so
  // this is the same archive, registered at context creation and
  // flushed on close.
  test('context_options_record_har', async ({ browser, baseURL }) => {
    const harPath = test.info().outputPath('recorded.har');
    const ctx = await browser.newContext({ recordHar: { path: harPath } });
    const p = await ctx.newPage();
    await p.goto(`${baseURL}/fx/landed`);
    // The archive is written on close, not before.
    await ctx.close();

    const har = JSON.parse(await fs.promises.readFile(harPath, 'utf8')) as {
      log: { creator: { name: string }; entries: { request: { url: string }; response: { status: number } }[] };
    };
    expect(har.log.creator.name).toBe('ferridriver');
    const landed = har.log.entries.find((e) => e.request.url.includes('/fx/landed'));
    expect(landed).toBeTruthy();
    expect(landed!.response.status).toBe(200);
  });

  test('context_options_record_har_url_filter_narrows_it', async ({ browser, baseURL }) => {
    const harPath = test.info().outputPath('filtered.har');
    // A RegExp, not a wire shape: the same union `route()` takes.
    const ctx = await browser.newContext({ recordHar: { path: harPath, urlFilter: /fx\/landed/ } });
    const p = await ctx.newPage();
    await p.goto(`${baseURL}/fx/landed`);
    await p.goto(`${baseURL}/fx/iframe`);
    await ctx.close();

    const har = JSON.parse(await fs.promises.readFile(harPath, 'utf8')) as {
      log: { entries: { request: { url: string } }[] };
    };
    const urls = har.log.entries.map((e) => e.request.url);
    expect(urls.some((u) => u.includes('/fx/landed'))).toBe(true);
    expect(urls.some((u) => u.includes('/fx/iframe'))).toBe(false);
  });

  // `recordHar`'s option fields were wired through and only the default
  // path had a test, which is the shape of a parity gap rather than a
  // feature: each field has an observable effect and none of them was
  // observed.
  test('context_options_record_har_content_and_mode', async ({ browser, baseURL }) => {
    const har = async (options: Record<string, unknown>, name: string) => {
      const harPath = test.info().outputPath(name);
      const ctx = await browser.newContext({ recordHar: { path: harPath, ...options } });
      const p = await ctx.newPage();
      await p.goto(`${baseURL}/fx/landed`);
      await ctx.close();
      const parsed = JSON.parse(await fs.promises.readFile(harPath, 'utf8')) as {
        log: {
          entries: {
            request: { url: string };
            timings: Record<string, number | null>;
            response: { content: { size: number; text?: string; _file?: string } };
          }[];
        };
      };
      const entry = parsed.log.entries.find((e) => e.request.url.includes('/fx/landed'))!;
      expect(entry).toBeTruthy();
      return { entry, harPath };
    };

    // embed (the default for a non-zip path): the body is inline.
    const embedded = await har({}, 'embed.har');
    expect(typeof embedded.entry.response.content.text).toBe('string');
    expect(embedded.entry.response.content.text!.length).toBeGreaterThan(0);

    // omit: no body at all, and the size collapses with it.
    const omitted = await har({ content: 'omit' }, 'omit.har');
    expect(omitted.entry.response.content.text).toBe(undefined);
    expect(omitted.entry.response.content.size).toBe(0);

    // omitContent is the deprecated spelling of the same thing.
    const legacy = await har({ omitContent: true }, 'legacy.har');
    expect(legacy.entry.response.content.text).toBe(undefined);

    // attach: the body moves to a `<sha1>.<ext>` file beside the archive.
    const attached = await har({ content: 'attach' }, 'attach.har');
    expect(attached.entry.response.content.text).toBe(undefined);
    const file = attached.entry.response.content._file;
    expect(typeof file).toBe('string');
    expect(file).toMatch(/^[0-9a-f]{40}\./);
    const beside = `${attached.harPath.slice(0, attached.harPath.lastIndexOf('/'))}/${file}`;
    expect((await fs.promises.readFile(beside, 'utf8')).length).toBeGreaterThan(0);

    // minimal: the timing breakdown is dropped, which is the whole
    // difference between the two modes.
    const full = await har({ mode: 'full' }, 'full.har');
    const minimal = await har({ mode: 'minimal' }, 'minimal.har');
    // Absent rather than null: Playwright's own unpopulated entry is
    // `{ send: -1, wait: -1, receive: -1 }` with no dns/connect/ssl keys
    // (`server/har/harTracer.ts:767`).
    expect('dns' in minimal.entry.timings).toBe(false);
    expect('connect' in minimal.entry.timings).toBe(false);
    expect(minimal.entry.timings.send).toBe(-1);
    expect(minimal.entry.timings.wait).toBe(-1);
    // Full keeps at least one of them as a real measurement.
    expect(full.entry.timings.send === -1 && full.entry.timings.wait === -1).toBe(false);
  });

  test('context_options_record_har_zip_attaches_bodies', async ({ browser, baseURL }) => {
    // A `.zip` path defaults to `attach` rather than `embed`, and packs
    // the archive as `har.har` plus one `<sha1>.<ext>` entry per body.
    const zipPath = test.info().outputPath('recorded.zip');
    const ctx = await browser.newContext({ recordHar: { path: zipPath } });
    const p = await ctx.newPage();
    await p.goto(`${baseURL}/fx/landed`);
    await ctx.close();

    const bytes = await fs.promises.readFile(zipPath);
    // Read the central directory's file names without a zip library:
    // every local entry starts with the PK\x03\x04 signature and carries
    // its name length at offset 26.
    const names: string[] = [];
    for (let i = 0; i + 30 < bytes.length; i++) {
      if (bytes[i] === 0x50 && bytes[i + 1] === 0x4b && bytes[i + 2] === 0x03 && bytes[i + 3] === 0x04) {
        const nameLen = bytes[i + 26] | (bytes[i + 27] << 8);
        names.push(new TextDecoder().decode(bytes.slice(i + 30, i + 30 + nameLen)));
      }
    }
    expect(names).toContain('har.har');
    expect(names.some((n) => /^[0-9a-f]{40}\./.test(n))).toBe(true);
  });

  // `strictSelectors` used to do nothing, and the two halves it
  // controls were both wrong in opposite directions: selector-taking
  // methods were ALWAYS strict (Playwright defaults them to false) and
  // Locator reads were NEVER strict (Playwright's locators always are).
  test('context_options_strict_selectors_off_by_default', async ({ browser }) => {
    const ctx = await browser.newContext({});
    try {
      const p = await ctx.newPage();
      await p.setContent('<div>a</div><div>b</div>');
      // Two matches, and the selector form takes the first.
      expect(await p.textContent('div')).toBe('a');
      // A Locator over the same selector is strict regardless.
      let message = '';
      try {
        await p.locator('div').textContent();
      } catch (e) {
        message = String((e as Error)?.message ?? e);
      }
      expect(message).toContain('strict mode violation');
    } finally {
      await ctx.close();
    }
  });

  test('context_options_strict_selectors_on', async ({ browser }) => {
    const ctx = await browser.newContext({ strictSelectors: true });
    try {
      const p = await ctx.newPage();
      await p.setContent('<div>a</div><div>b</div>');
      for (const call of [() => p.textContent('div'), () => p.click('div', { timeout: 2000 })]) {
        let message = '';
        try {
          await call();
        } catch (e) {
          message = String((e as Error)?.message ?? e);
        }
        expect(message).toContain('strict mode violation');
      }
      // A selector matching exactly one element is unaffected.
      expect(await p.textContent('div:nth-of-type(2)')).toBe('b');
    } finally {
      await ctx.close();
    }
  });

  // `acceptDownloads` used to do nothing on any backend. Two senders
  // were fighting over the one `setDownloadBehavior` each context gets:
  // an explicit `deny` from the options bag, then an unconditional
  // `allow` from the lazy path that runs the moment anything registers
  // interest in downloads. Whichever landed last won, and it was
  // usually the wrong one.
  test('context_options_accept_downloads_denies', async ({ browser, baseURL }) => {
    const ctx = await browser.newContext({ acceptDownloads: false });
    try {
      const p = await ctx.newPage();
      await p.goto(`${baseURL}/fx/iframe`);
      await p.evaluate(
        `const a = document.createElement('a'); a.id = 'dl'; a.href = '/fx/download'; a.textContent = 'dl'; document.body.appendChild(a); null`,
      );
      const downloadPromise = p.waitForEvent('download', { timeout: 15000 });
      await p.click('#dl');
      const dl = (await downloadPromise) as { path(): Promise<string>; failure(): Promise<string | null> };

      // The refusal has to name the option that caused it, identically
      // on every engine: Chromium and Firefox report `canceled`, WebKit
      // `cancelled`, and none of them says why.
      const failure = await dl.failure();
      expect(failure).toBe('Pass { acceptDownloads: true } when you are creating your browser context.');
      let message = '';
      try {
        await dl.path();
      } catch (e) {
        message = String((e as Error)?.message ?? e);
      }
      expect(message).toContain('acceptDownloads');
    } finally {
      await ctx.close();
    }
  });

  test('context_options_accept_downloads_allows', async ({ browser, baseURL }) => {
    // The other half: the default still writes the file. Without this
    // a backend that refused everything would pass the test above.
    const ctx = await browser.newContext({ acceptDownloads: true });
    try {
      const p = await ctx.newPage();
      await p.goto(`${baseURL}/fx/iframe`);
      await p.evaluate(
        `const a = document.createElement('a'); a.id = 'dl'; a.href = '/fx/download'; a.textContent = 'dl'; document.body.appendChild(a); null`,
      );
      const downloadPromise = p.waitForEvent('download', { timeout: 15000 });
      await p.click('#dl');
      const dl = (await downloadPromise) as { path(): Promise<string>; failure(): Promise<string | null> };
      expect(await dl.failure()).toBe(null);
      expect((await dl.path()).length).toBeGreaterThan(0);
    } finally {
      await ctx.close();
    }
  });

  // `screen` works on every engine, Firefox included: BiDi has
  // `emulation.setScreenSettingsOverride`, which Playwright's own BiDi
  // backend uses (`bidi/bidiBrowser.ts::doUpdateDefaultViewport`). It
  // used to be refused here as unsupported.
  test('context_options_screen', async ({ browser }) => {
    const ctx = await browser.newContext({
      viewport: { width: 640, height: 480 },
      screen: { width: 1920, height: 1080 },
    });
    try {
      const p = await ctx.newPage();
      const dims = (await p.evaluate(() => ({ sw: window.screen.width, sh: window.screen.height }))) as {
        sw: number;
        sh: number;
      };
      expect(dims.sw).toBe(1920);
      expect(dims.sh).toBe(1080);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_bypass_csp', async ({ browser, baseURL, browserName }) => {
    if (browserName === 'firefox') {
      await expectOptionUnsupportedOnFirefox(browser, { bypassCSP: true }, 'bypassCSP');
      return;
    }
    // /fx/csp serves script-src 'none'; with bypassCSP the init script
    // still runs and sets the window flag.
    const ctx = await browser.newContext({ bypassCSP: true });
    try {
      const p = await ctx.newPage();
      await p.addInitScript(() => {
        (window as unknown as { __fd_csp_bypass: string }).__fd_csp_bypass = 'yes';
      });
      await p.goto(`${baseURL}/fx/csp`);
      expect(await p.evaluate('window.__fd_csp_bypass || null')).toBe('yes');
    } finally {
      await ctx.close();
    }
  });

  test('context_options_base_url', async ({ browser, baseURL }) => {
    // Relative-URL resolution is purely client-side — the backend only
    // sees the already-resolved absolute URL.
    const ctx = await browser.newContext({ baseURL });
    try {
      const p = await ctx.newPage();
      await p.goto('/fx/landed');
      const body = (await p.evaluate(() => document.body.textContent)) as string;
      expect(body.includes('landed')).toBe(true);
      expect(p.url()).toBe(`${baseURL}/fx/landed`);
    } finally {
      await ctx.close();
    }
  });

  test('context_options_storage_state', async ({ browser, baseURL, browserName }) => {
    const origin = new URL(baseURL!).origin;
    const ctx = await browser.newContext({
      storageState: {
        cookies: [
          {
            name: 'ferri_ck',
            value: 'hello',
            domain: '127.0.0.1',
            path: '/',
            secure: false,
            httpOnly: false,
            expires: -1,
            sameSite: 'Lax',
          },
        ],
        origins: [{ origin, localStorage: [{ name: 'ferri_ls', value: 'world' }] }],
      },
    });
    try {
      const p = await ctx.newPage();
      await p.goto(`${origin}/fx/landed`);
      const got = (await p.evaluate(() => ({
        ck: document.cookie,
        ls: localStorage.getItem('ferri_ls'),
      }))) as { ck: string; ls: string | null };
      // WebKit/BiDi cookie stores may not accept secure:false +
      // hostname-only 127.0.0.1 domains the way CDP does; the cookie
      // assert is chromium-scoped, localStorage restoration is not.
      if (browserName === 'chromium') {
        expect(got.ck.includes('ferri_ck=hello')).toBe(true);
      }
      expect(got.ls).toBe('world');
    } finally {
      await ctx.close();
    }
  });

  test('context_options_proxy', async ({ browser, baseURL, request }) => {
    // The fixture server's observable proxy answers every absolute-form
    // request with PROXY:ok and records the request line. Navigating to
    // a fake non-loopback host proves traversal without depending on
    // per-engine loopback bypass rules (macOS WebKit never proxies
    // 127.0.0.1; Chromium needs <-loopback>); with an HTTP proxy set
    // the browser sends the absolute-form request without resolving
    // DNS.
    await fxProxyLogReset(request, baseURL);
    const proxyServer = await fxProxyUrl(request, baseURL);
    const ctx = await browser.newContext({ proxy: { server: proxyServer } });
    try {
      const p = await ctx.newPage();
      await p.goto('http://ferri-proxy.test/behind-proxy');
      const body = (await p.evaluate(() => document.body.textContent)) as string;
      expect(body.includes('PROXY:ok')).toBe(true);
      const log = await fxProxyLog(request, baseURL);
      expect(log.hits).toBeGreaterThanOrEqual(1);
      expect(log.lines.some((l) => l.includes('ferri-proxy.test') && l.includes('behind-proxy'))).toBe(true);
    } finally {
      await ctx.close();
    }
  });
});
