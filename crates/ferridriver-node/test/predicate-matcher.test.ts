/**
 * Predicate-matcher parity: page.route / page.unroute / page.waitForRequest
 * / page.waitForResponse accept a function predicate, not just
 * string | RegExp. The route predicate receives a `URL`; the waitFor*
 * predicates receive a live `Request` / `Response` and may return
 * `boolean | Promise<boolean>`.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";
import { createServer, type Server, type IncomingMessage, type ServerResponse } from "node:http";

const SERVE: Record<string, { body: string; type: string }> = {
  "/": { body: `<!DOCTYPE html><html><head><title>home</title></head><body><h1>x</h1></body></html>`, type: "text/html" },
  "/api/users": { body: `{"users":["real"]}`, type: "application/json" },
  "/api/posts": { body: `{"posts":["real"]}`, type: "application/json" },
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

for (const backend of BACKENDS) {
  describe(`[${backend}] predicate matchers`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPageWithUrl(testUrl + "/");
    });

    afterAll(async () => {
      await browser.close();
    });

    it("route(predicate) gets a URL and intercepts; unroute(predicate) stops it", async () => {
      // Predicate receives a real URL object — `url.pathname` must work.
      const pred = (url: URL) => url.pathname === "/api/users";
      await page.route(pred, (route) => {
        route.fulfill({
          status: 200,
          body: '{"users":["mocked"]}',
          contentType: "application/json",
        });
      });

      const mocked = await page.evaluate("fetch('/api/users').then(r => r.text())");
      expect(String(mocked)).toContain('"mocked"');

      await page.unroute(pred);
      const real = await page.evaluate("fetch('/api/users').then(r => r.text())");
      expect(String(real)).toContain('"real"');
    });

    it("waitForRequest(predicate) resolves with the matching live Request", async () => {
      const reqP = page.waitForRequest((r) => r.url().includes("/api/posts"));
      await page.evaluate("fetch('/api/posts').catch(() => {})");
      const req = await reqP;
      expect(req.url()).toContain("/api/posts");
    });

    it("waitForResponse(async predicate) resolves with the matching Response", async () => {
      const resP = page.waitForResponse(
        async (res) => res.url().includes("/api/users") && res.status() === 200
      );
      await page.evaluate("fetch('/api/users').catch(() => {})");
      const res = await resP;
      expect(res.status()).toBe(200);
      expect(res.url()).toContain("/api/users");
    });
  });
}
