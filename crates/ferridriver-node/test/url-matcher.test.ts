/**
 * URL matcher parity (task #6): page.route / page.unroute /
 * page.waitForUrl / page.waitForRequest / page.waitForResponse all accept
 * the unified UrlMatcher shape — glob string OR { regexSource, regexFlags }.
 *
 * Playwright signature (cloned at
 * /tmp/playwright/packages/playwright-core/src/client/page.ts):
 *
 *   route(url: URLMatch, handler, options?)
 *   waitForURL(url: URLMatch, options?)
 *   waitForRequest(urlOrPredicate: string | RegExp | predicate, options?)
 *   waitForResponse(urlOrPredicate: string | RegExp | predicate, options?)
 *
 *   URLMatch = string | RegExp | ((url: URL) => boolean) | URLPattern
 *
 * ferridriver NAPI lowers RegExp into { regexSource, regexFlags } since
 * napi-rs cannot bind a JS RegExp directly; predicates stay on the TS
 * wrapper side. This test drives the NAPI surface directly with the
 * lowered form to prove Rust-side parity on every matcher variant and on
 * every matcher-accepting call site.
 */
import { describe, it, expect, beforeAll, afterAll, afterEach } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type Server, type IncomingMessage, type ServerResponse } from "node:http";

const SERVE: Record<string, { body: string; type: string }> = {
  "/": { body: `<!DOCTYPE html><html><head><title>home</title></head><body><h1>origin</h1></body></html>`, type: "text/html" },
  "/page-a": { body: `<!DOCTYPE html><html><head><title>A</title></head><body><h1>A</h1></body></html>`, type: "text/html" },
  "/page-b": { body: `<!DOCTYPE html><html><head><title>B</title></head><body><h1>B</h1></body></html>`, type: "text/html" },
  "/api/users": { body: `{"users":["real"]}`, type: "application/json" },
  "/api/posts": { body: `{"posts":["real"]}`, type: "application/json" },
  "/api/users/123": { body: `{"id":123}`, type: "application/json" },
  "/static/logo.png": { body: "binary-placeholder", type: "image/png" },
};

let testServer: Server;
let testUrl: string;

beforeAll(async () => {
  testServer = createServer((req: IncomingMessage, res: ServerResponse) => {
    const entry = SERVE[req.url ?? ""];
    if (!entry) {
      res.writeHead(404);
      res.end("not found");
      return;
    }
    res.writeHead(200, { "Content-Type": entry.type });
    res.end(entry.body);
  });
  await new Promise<void>((resolve) => {
    testServer.listen(0, "127.0.0.1", () => {
      const addr = testServer.address() as { port: number };
      testUrl = `http://127.0.0.1:${addr.port}`;
      resolve();
    });
  });
});

afterAll(() => {
  testServer?.close();
});

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw"];

type Matcher = string | RegExp;

for (const backend of BACKENDS) {
  describe(`[${backend}] UrlMatcher parity`, () => {
    let browser: Browser;
    let page: Page;
    // Every registration goes through routeTracked so cleanup is unconditional.
    const live: Matcher[] = [];

    const routeTracked = async (
      matcher: Matcher,
      handler: Parameters<Page["route"]>[1]
    ) => {
      live.push(matcher);
      await page.route(matcher, handler);
    };

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPageWithUrl(testUrl + "/");
    });

    afterAll(async () => {
      await browser.close();
    });

    afterEach(async () => {
      // Unroute every pattern registered in the previous test, whether
      // the test passed, failed, or threw before assertion. Errors are
      // tolerated because a test may have already unrouted explicitly.
      for (const m of live.splice(0)) {
        await page.unroute(m).catch(() => {});
      }
    });

    // ── Glob matcher (the common case) ─────────────────────────────────

    it("glob string intercepts matching URLs", async () => {
      await routeTracked("**/api/users", (route) => {
        route.fulfill({
          status: 200,
          body: '{"users":["mocked"]}',
          contentType: "application/json",
        });
      });

      const text = await page.evaluate(
        "fetch('/api/users').then(r => r.text())"
      );
      expect(String(text)).toContain('"mocked"');
    });

    it("glob with brace alternation {png,jpg} expands to regex alternation", async () => {
      let blocked = false;
      await routeTracked("**/*.{png,jpg}", (route) => {
        blocked = true;
        route.abort("blockedbyclient");
      });

      const outcome = await page.evaluate(
        "fetch('/static/logo.png').then(() => 'ok').catch(() => 'blocked')"
      );
      expect(outcome).toBe("blocked");
      expect(blocked).toBe(true);
    });

    it("glob single-star does NOT cross path segments", async () => {
      // Playwright parity: `**/api/*` matches `/api/users` (one segment
      // after /api/) but NOT `/api/users/123` (two segments).
      const hits: string[] = [];
      await routeTracked("**/api/*", (route) => {
        hits.push(route.url);
        route.fulfill({ status: 200, body: "{}", contentType: "application/json" });
      });

      await page.evaluate("fetch('/api/users').then(r => r.text())");
      await page.evaluate(
        "fetch('/api/users/123').then(r => r.text()).catch(() => '')"
      );

      expect(hits.some((u) => u.endsWith("/api/users"))).toBe(true);
      expect(hits.some((u) => u.endsWith("/api/users/123"))).toBe(false);
    });

    // ── Regex matcher (real JS RegExp literal) ─────────────────────────

    it("RegExp /.../i is case-insensitive", async () => {
      await routeTracked(/\/API\/USERS/i, (route) => {
        route.fulfill({
          status: 200,
          body: '{"users":["regex"]}',
          contentType: "application/json",
        });
      });

      const text = await page.evaluate(
        "fetch('/api/users').then(r => r.text())"
      );
      expect(String(text)).toContain('"regex"');
    });

    it("RegExp without 'i' flag is case-sensitive (uppercase source misses lowercase URL)", async () => {
      let hit = false;
      await routeTracked(/\/API\/USERS/, (route) => {
        hit = true;
        route.fulfill({ status: 200, body: "{}", contentType: "application/json" });
      });

      const text = await page.evaluate(
        "fetch('/api/users').then(r => r.text())"
      );
      expect(hit).toBe(false);
      expect(String(text)).toContain('"real"');
    });

    it("RegExp with unsupported flag ('y' sticky) is rejected at NAPI boundary", async () => {
      // `y` (sticky) has no Rust regex equivalent; surface the rejection
      // instead of silently shipping a broken regex.
      await expect(page.route(/x/y, () => {})).rejects.toThrow(/unsupported JS regex flag/);
    });

    // ── waitForUrl ─────────────────────────────────────────────────────

    it("waitForUrl resolves on glob match after navigation", async () => {
      const nav = page.goto(testUrl + "/page-a", null);
      await page.waitForUrl("**/page-a");
      await nav;
      expect(await page.title()).toBe("A");
    });

    it("waitForUrl resolves on RegExp match after navigation", async () => {
      const nav = page.goto(testUrl + "/page-b", null);
      await page.waitForUrl(/\/page-[ab]$/);
      await nav;
      expect(await page.title()).toBe("B");
    });

    // ── waitForRequest + waitForResponse ──────────────────────────────

    it("waitForRequest resolves when a matching glob request is made", async () => {
      // Fire the request *after* setting up the waiter.
      const waiter = page.waitForRequest("**/api/posts", 3000);
      setTimeout(() => {
        page.evaluate("fetch('/api/posts').then(r => r.text())").catch(() => {});
      }, 50);
      const req = await waiter;
      expect(req.url()).toContain("/api/posts");
      expect(req.method()).toBe("GET");
    });

    it("waitForResponse resolves when a matching RegExp response is observed", async () => {
      const waiter = page.waitForResponse(/\/api\/posts$/, 3000);
      setTimeout(() => {
        page.evaluate("fetch('/api/posts').then(r => r.text())").catch(() => {});
      }, 50);
      const resp = await waiter;
      expect(resp.status()).toBe(200);
      expect(resp.url()).toContain("/api/posts");
    });

    // ── unroute ────────────────────────────────────────────────────────

    it("unroute with equal glob string retires only the matching registration", async () => {
      let hits = 0;
      await routeTracked("**/api/users", () => {
        hits++;
      });
      // Fire once so we know the handler ran once.
      await page.evaluate("fetch('/api/users').then(r => r.text())");
      const beforeUnroute = hits;
      expect(beforeUnroute).toBeGreaterThanOrEqual(1);

      await page.unroute("**/api/users");
      // Drop from live so afterEach doesn't double-unroute.
      live.length = 0;

      await page.evaluate("fetch('/api/users').then(r => r.text())");
      expect(hits).toBe(beforeUnroute);
    });

    it("unroute with the same RegExp source retires a regex registration", async () => {
      // Use two distinct RegExp objects with the same source+flags —
      // matches Playwright's urlMatchesEqual which compares source+flags,
      // not reference identity.
      let hits = 0;
      await routeTracked(/\/api\/posts/, () => {
        hits++;
      });
      await page.evaluate("fetch('/api/posts').then(r => r.text())");
      const before = hits;
      expect(before).toBeGreaterThanOrEqual(1);

      await page.unroute(/\/api\/posts/);
      live.length = 0;

      await page.evaluate("fetch('/api/posts').then(r => r.text())");
      expect(hits).toBe(before);
    });
  });
}
