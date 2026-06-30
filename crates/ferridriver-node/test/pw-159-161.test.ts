// NAPI coverage for Playwright 1.59-1.61 gap fills:
// webError.location() (1.60), request.existingResponse() (1.59),
// page.localStorage / page.sessionStorage WebStorage (1.61),
// apiResponse.serverAddr() (1.61), BrowserContext lifecycle-mirror
// events (1.60): framenavigated / pageload / pageclose via on + waitForEvent.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { tmpdir } from "os";
import { join } from "path";
import { readFileSync, rmSync } from "fs";
import { type Browser, type Page, HttpClient } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`pw 1.59-1.61 [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("request.existingResponse() returns the received response without waiting", async () => {
      const resp = await page.goto("data:text/html,<title>existing</title>");
      const req = resp!.request();
      const existing = await req.existingResponse();
      expect(existing).not.toBeNull();
      expect(existing!.url()).toBe(resp!.url());
    });

    it("webError.location() exposes { url, line, column }", async () => {
      const context = browser.defaultContext();
      const [werr] = await Promise.all([
        context.waitForEvent("weberror", 5000),
        page.evaluate(() => {
          setTimeout(() => {
            throw new Error("boom-loc");
          }, 10);
        }),
      ]);
      expect(werr.error().message).toBe("boom-loc");
      const loc = werr.location();
      expect(typeof loc.url).toBe("string");
      expect(typeof loc.line).toBe("number");
      expect(typeof loc.column).toBe("number");
    });

    it("apiResponse.serverAddr() reports the resolved peer address", async () => {
      const server = Bun.serve({ port: 0, fetch: () => new Response("ok") });
      try {
        const client = HttpClient.create();
        const resp = await client.get(`http://127.0.0.1:${server.port}/api`);
        expect(resp.status).toBe(200);
        const addr = resp.serverAddr();
        expect(addr).not.toBeNull();
        expect(addr!.ipAddress).toBe("127.0.0.1");
        expect(addr!.port).toBe(server.port);
      } finally {
        server.stop(true);
      }
    });

    it("context.waitForEvent('framenavigated') resolves a Frame", async () => {
      const context = browser.defaultContext();
      const [frame] = await Promise.all([
        context.waitForEvent("framenavigated", 5000),
        page.goto("data:text/html,<title>ctx-navmark</title>"),
      ]);
      expect(typeof (frame as any).url).toBe("function");
      expect((frame as any).url()).toContain("ctx-navmark");
    });

    it("context.waitForEvent('pageload') resolves a Page", async () => {
      const context = browser.defaultContext();
      const [p] = await Promise.all([
        context.waitForEvent("pageload", 5000),
        page.goto("data:text/html,<title>ctx-loadmark</title>"),
      ]);
      expect(typeof (p as any).url).toBe("function");
      expect((p as any).url()).toContain("ctx-loadmark");
    });

    it("context.once('framenavigated') delivers a Frame to the listener", async () => {
      const context = browser.defaultContext();
      const got = new Promise<string>((resolve) => {
        context.once("framenavigated", (frame: any) => resolve(frame.url()));
      });
      await page.goto("data:text/html,<title>once-nav</title>");
      expect(await got).toContain("once-nav");
    });

    it("context.waitForEvent('pageclose') resolves the closed Page", async () => {
      const context = browser.defaultContext();
      const newPage = await context.newPage();
      const [closed] = await Promise.all([
        context.waitForEvent("pageclose", 5000),
        newPage.close(),
      ]);
      expect((closed as any).isClosed()).toBe(true);
    });

    it("browser.waitForEvent('context') resolves the new BrowserContext", async () => {
      // newContext() is synchronous, so arm the wait and let it subscribe
      // (a real async tick) before triggering — otherwise the emit can
      // outrun the lazily-polled waitForEvent future on a multi-thread
      // runtime.
      const waitP = browser.waitForEvent("context", 5000);
      await page.evaluate(() => 1);
      browser.newContext();
      const bcx = await waitP;
      expect(typeof (bcx as any).newPage).toBe("function");
      expect(typeof (bcx as any).cookies).toBe("function");
    });

    it("browser.once('context') delivers the context to the listener", async () => {
      const got = new Promise<boolean>((resolve) => {
        browser.once("context", (bcx: any) => resolve(typeof bcx.newPage === "function"));
      });
      browser.newContext();
      expect(await got).toBe(true);
    });

    it("context.tracing.startHar/stopHar records network into a HAR file", async () => {
      const server = Bun.serve({
        port: 0,
        fetch: () => new Response("<!doctype html><body>har</body>", { headers: { "content-type": "text/html" } }),
      });
      const harPath = join(tmpdir(), `ferri-har-napi-${backend}-${server.port}.har`);
      try {
        const url = `http://127.0.0.1:${server.port}/page`;
        await page.context().tracing.startHar(harPath);
        await page.goto(url);
        await page.goto(`${url}?second`);
        await page.context().tracing.stopHar();

        const har = JSON.parse(readFileSync(harPath, "utf8"));
        const urls: string[] = har.log.entries.map((e: any) => e.request.url);
        expect(urls.some((u) => u.includes(`127.0.0.1:${server.port}`))).toBe(true);
        expect(har.log.entries.some((e: any) => e.response.status === 200)).toBe(true);

        // trace .zip recorder is not implemented.
        await expect(page.context().tracing.start()).rejects.toThrow();
      } finally {
        server.stop(true);
        rmSync(harPath, { force: true });
      }
    });

    it("page.localStorage / sessionStorage round-trip against real storage", async () => {
      const server = Bun.serve({
        port: 0,
        fetch: () =>
          new Response("<!doctype html><body>ws</body>", {
            headers: { "content-type": "text/html" },
          }),
      });
      try {
        await page.goto(`http://localhost:${server.port}/store`);
        await page.localStorage.setItem("token", "abc");
        await page.localStorage.setItem("user", "sam");
        await page.sessionStorage.setItem("sid", "sess-1");

        expect(await page.localStorage.getItem("token")).toBe("abc");
        expect(await page.localStorage.getItem("nope")).toBeNull();

        const names = (await page.localStorage.items()).map((i) => i.name).sort();
        expect(names).toEqual(["token", "user"]);

        expect(await page.evaluate(() => window.localStorage.getItem("token"))).toBe("abc");
        expect(await page.evaluate(() => window.sessionStorage.getItem("sid"))).toBe("sess-1");

        await page.localStorage.removeItem("user");
        expect((await page.localStorage.items()).map((i) => i.name)).toEqual(["token"]);

        await page.localStorage.clear();
        expect((await page.localStorage.items()).length).toBe(0);
      } finally {
        server.stop(true);
      }
    });
  });
}
