/**
 * §3.1 NAPI parity tests: `page.goto` / `reload` / `goBack` /
 * `goForward` return a live `Response` object across supported
 * backends. Mirrors the per-backend QuickJS integration tests in
 * `crates/ferridriver-cli/tests/backends_support/navigation_response.rs`
 * from the JS side.
 *
 * CDP backends observe the main-document response via CDP
 * `Network.responseReceived`, so every assertion runs end-to-end. The
 * generated `index.d.ts` declares the return type as `Promise<Response |
 * null>` exactly like Playwright's `page.goto`.
 *
 * WebKit's stock `WKWebView` has no public API for main-document
 * response headers/status (documented in the §1.4 gap matrix); the NAPI
 * tests gate to `cdp-pipe`/`cdp-raw`. WebKit honesty is verified by the
 * Rust-side integration suite which explicitly asserts `null` rather
 * than skipping.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";

const SERVE: Record<string, (req: IncomingMessage, res: ServerResponse) => void> = {
  "/landed": (_req, res) => {
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.end("landed");
  },
  "/redirect": (_req, res) => {
    res.writeHead(302, { Location: "/landed" });
    res.end();
  },
  "/api/users": (_req, res) => {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ users: ["alice", "bob"] }));
  },
  "/not-found": (_req, res) => {
    res.writeHead(404, { "Content-Type": "text/plain" });
    res.end("nope");
  },
};

let httpServer: Server;
let baseUrl: string;

beforeAll(async () => {
  httpServer = createServer((req, res) => {
    const handler = SERVE[req.url ?? ""];
    if (handler) {
      handler(req, res);
    } else {
      res.writeHead(404);
      res.end("not found");
    }
  });
  await new Promise<void>((resolve) => {
    httpServer.listen(0, "127.0.0.1", () => {
      const addr = httpServer.address() as AddressInfo;
      baseUrl = `http://127.0.0.1:${addr.port}`;
      resolve();
    });
  });
});

afterAll(() => {
  httpServer?.close();
});

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

for (const backend of BACKENDS) {
  describe(`[${backend}] Navigation returns Response (§3.1)`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("page.goto returns the main-document Response", async () => {
      const resp = await page.goto(`${baseUrl}/landed`, null);
      expect(resp).not.toBeNull();
      expect(resp!.status()).toBe(200);
      expect(resp!.ok()).toBe(true);
      expect(resp!.url()).toContain("/landed");
    });

    it("page.goto follows redirects and returns the landed Response", async () => {
      const resp = await page.goto(`${baseUrl}/redirect`, null);
      expect(resp).not.toBeNull();
      expect(resp!.status()).toBe(200);
      expect(resp!.url()).toContain("/landed");
    });

    it("page.goto surfaces non-2xx status in the Response (does not throw)", async () => {
      const resp = await page.goto(`${baseUrl}/not-found`, null);
      expect(resp).not.toBeNull();
      expect(resp!.status()).toBe(404);
      expect(resp!.ok()).toBe(false);
    });

    it("page.reload returns the main-document Response", async () => {
      await page.goto(`${baseUrl}/landed`, null);
      const resp = await page.reload(null);
      expect(resp).not.toBeNull();
      expect(resp!.status()).toBe(200);
      expect(resp!.ok()).toBe(true);
      expect(resp!.url()).toContain("/landed");
    });

    it("page.goBack and page.goForward return the target history Response", async () => {
      await page.goto(`${baseUrl}/landed`, null);
      await page.goto(`${baseUrl}/api/users`, null);
      const back = await page.goBack(null);
      expect(back).not.toBeNull();
      expect(back!.status()).toBe(200);
      expect(back!.url()).toContain("/landed");
      const fwd = await page.goForward(null);
      expect(fwd).not.toBeNull();
      expect(fwd!.status()).toBe(200);
      expect(fwd!.url()).toContain("/api/users");
    });

    it("page.goto rejects for unreachable URLs", async () => {
      await expect(page.goto("http://127.0.0.1:65532/unreachable", null)).rejects.toThrow();
    });
  });
}
