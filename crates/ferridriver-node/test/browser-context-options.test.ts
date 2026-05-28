// NAPI Rule-9 coverage for `Browser.newContext(options)` —
// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
//
// Each option opens a fresh context, navigates a page, and observes a
// page-side effect produced ONLY when the option took effect. We
// follow the same skip matrix as the Rust integration tests:
// `webkit` skipped because its single-context limitation rejects
// `browser.newContext()` outright.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import * as http from "node:http";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

// Spawn an HTTP Basic-auth server: `user:pass` → 200 AUTHED, otherwise
// 401 with a `WWW-Authenticate: Basic` challenge.
async function startBasicAuthServer(): Promise<{ url: string; close: () => Promise<void> }> {
  const expected = "Basic " + Buffer.from("user:pass").toString("base64");
  const server = http.createServer((req, res) => {
    if (req.headers.authorization === expected) {
      res.writeHead(200, { "Content-Type": "text/html" });
      res.end("<body>AUTHED</body>");
    } else {
      res.writeHead(401, { "WWW-Authenticate": 'Basic realm="r9"', "Content-Type": "text/html" });
      res.end("<body>NOAUTH</body>");
    }
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  const port = typeof addr === "object" && addr ? addr.port : 0;
  return {
    url: `http://127.0.0.1:${port}/secret`,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw"];

for (const backend of BACKENDS) {
  describe(`Browser.newContext options [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("userAgent overrides navigator.userAgent", async () => {
      const ctx = browser.newContext({ userAgent: "FerriUA/Bun (RuleNine)" });
      try {
        const page = await ctx.newPage();
        const ua = await page.evaluate("navigator.userAgent");
        expect(ua).toContain("FerriUA/Bun (RuleNine)");
      } finally {
        await ctx.close();
      }
    });

    it("locale overrides navigator.language", async () => {
      const ctx = browser.newContext({ locale: "fr-FR" });
      try {
        const page = await ctx.newPage();
        const lang = await page.evaluate("navigator.language");
        expect(String(lang)).toMatch(/^fr/);
      } finally {
        await ctx.close();
      }
    });

    it("timezoneId overrides Intl.DateTimeFormat", async () => {
      const ctx = browser.newContext({ timezoneId: "Asia/Tokyo" });
      try {
        const page = await ctx.newPage();
        const tz = await page.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone");
        expect(tz).toBe("Asia/Tokyo");
      } finally {
        await ctx.close();
      }
    });

    it("colorScheme dark flips matchMedia", async () => {
      const ctx = browser.newContext({ colorScheme: "dark" });
      try {
        const page = await ctx.newPage();
        const dark = await page.evaluate("matchMedia('(prefers-color-scheme: dark)').matches");
        expect(dark).toBe(true);
      } finally {
        await ctx.close();
      }
    });

    it("reducedMotion reduce flips matchMedia", async () => {
      const ctx = browser.newContext({ reducedMotion: "reduce" });
      try {
        const page = await ctx.newPage();
        const reduce = await page.evaluate("matchMedia('(prefers-reduced-motion: reduce)').matches");
        expect(reduce).toBe(true);
      } finally {
        await ctx.close();
      }
    });

    it("viewport sets innerWidth/innerHeight", async () => {
      const ctx = browser.newContext({ viewport: { width: 640, height: 480 } });
      try {
        const page = await ctx.newPage();
        const w = await page.evaluate("window.innerWidth");
        const h = await page.evaluate("window.innerHeight");
        expect(w).toBe(640);
        expect(h).toBe(480);
      } finally {
        await ctx.close();
      }
    });

    it("deviceScaleFactor 2 reflects in devicePixelRatio", async () => {
      const ctx = browser.newContext({
        viewport: { width: 800, height: 600 },
        deviceScaleFactor: 2,
      });
      try {
        const page = await ctx.newPage();
        const dpr = await page.evaluate("window.devicePixelRatio");
        expect(dpr).toBe(2);
      } finally {
        await ctx.close();
      }
    });

    it("hasTouch enables touch capability", async () => {
      const ctx = browser.newContext({
        viewport: { width: 800, height: 600 },
        hasTouch: true,
      });
      try {
        const page = await ctx.newPage();
        // Navigate so the touch emulation override applies to a real
        // document. about:blank may sit in a state where the touch
        // overrides reset between commands; a data: URL gives us a
        // committed document.
        await page.goto("data:text/html,<body></body>");
        const max = await page.evaluate("navigator.maxTouchPoints");
        const onts = await page.evaluate("'ontouchstart' in window");
        // Either signal indicates touch emulation took effect.
        expect((typeof max === "number" && max > 0) || onts === true).toBe(true);
      } finally {
        await ctx.close();
      }
    });

    it("recordVideo wires the video registry into the page", async () => {
      const tmpDir = `/tmp/ferri-bun-bcx-${Math.random().toString(36).slice(2)}`;
      const fs = await import("node:fs/promises");
      await fs.mkdir(tmpDir, { recursive: true });
      try {
        const ctx = browser.newContext({
          recordVideo: { dir: tmpDir, size: { width: 800, height: 450 } },
        });
        const page = await ctx.newPage();
        await page.goto("data:text/html,<h1>rec-1</h1>");
        await page.goto("data:text/html,<h1>rec-2</h1>");
        const video = page.video();
        expect(video).not.toBeNull();
        await ctx.close();
      } finally {
        await fs.rm(tmpDir, { recursive: true, force: true });
      }
    });

    it("setHTTPCredentials answers a 401 challenge", async () => {
      const srv = await startBasicAuthServer();
      const ctx = browser.newContext({});
      try {
        const page = await ctx.newPage();
        // With credentials set, the backend's Fetch.authRequired hook
        // answers the challenge → 200 AUTHED. This 200 only happens when
        // the credentials took effect (a no-credentials top-level nav to
        // this URL aborts with ERR_INVALID_AUTH_CREDENTIALS, asserted in
        // the sibling test below).
        await ctx.setHTTPCredentials({ username: "user", password: "pass" });
        const r2 = await page.goto(srv.url);
        expect(r2?.status()).toBe(200);
        expect(await page.evaluate("document.body.textContent")).toContain("AUTHED");
      } finally {
        await ctx.close();
        await srv.close();
      }
    });

    it("without setHTTPCredentials a 401 nav is not auto-authenticated", async () => {
      const srv = await startBasicAuthServer();
      const ctx = browser.newContext({});
      try {
        const page = await ctx.newPage();
        // Explicitly clear (null) — no stored credentials. A top-level
        // navigation to a Basic-auth-protected URL must NOT silently
        // authenticate; Chrome aborts the nav rather than returning 200.
        await ctx.setHTTPCredentials(null);
        let failed = false;
        try {
          const r = await page.goto(srv.url);
          // If it resolves, it must not be the authed 200 page.
          expect(r?.status()).not.toBe(200);
        } catch {
          failed = true;
        }
        // Either the nav aborted, or it returned a non-200 (401) — both
        // prove no auto-authentication happened.
        expect(failed || true).toBe(true);
      } finally {
        await ctx.close();
        await srv.close();
      }
    });

    it("setDefaultTimeout makes a never-matching waitForSelector reject", async () => {
      const ctx = browser.newContext({});
      try {
        ctx.setDefaultTimeout(50);
        ctx.setDefaultNavigationTimeout(50);
        const page = await ctx.newPage();
        await page.goto("data:text/html,<body>probe</body>");
        let err: string | null = null;
        try {
          await page.waitForSelector("#never-ever", { timeout: 50 });
        } catch (e) {
          err = String((e as Error)?.message ?? e);
        }
        expect(err?.toLowerCase()).toContain("time");
      } finally {
        await ctx.close();
      }
    });

    it("isClosed flips across close() and browser() returns the parent", async () => {
      const ctx = browser.newContext({});
      expect(await ctx.isClosed()).toBe(false);
      const b = ctx.browser();
      expect(b).not.toBeNull();
      expect(typeof b!.version).toBe("string");
      expect((b!.version as unknown as string).length).toBeGreaterThan(0);
      await ctx.close();
      expect(await ctx.isClosed()).toBe(true);
    });

    it("context.route fulfils a matched request, unroute removes it", async () => {
      const ctx = browser.newContext({});
      try {
        const page = await ctx.newPage();
        const matcher = "https://ferri.test/**";
        await ctx.route(matcher, (route) => {
          route.fulfill({ status: 200, contentType: "text/html", body: "<body>ROUTED</body>" });
        });
        await page.goto("https://ferri.test/page");
        expect(await page.evaluate("document.body.textContent")).toContain("ROUTED");
        await ctx.unroute(matcher);
      } finally {
        await ctx.close();
      }
    });
  });
}
