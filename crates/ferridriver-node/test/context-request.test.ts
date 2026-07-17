// NAPI coverage for the context-bound API request client
// (Playwright: `page.request` / `context.request` share the browser
// context's cookie jar — client/browserContext.ts:76,
// server/fetch.ts:649). Each test observes a cookie crossing the
// browser<->client boundary against a local Bun server.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`context-bound request [${backend}]`, () => {
    let browser: Browser;
    let page: Page;
    let base: string;
    let server: ReturnType<typeof Bun.serve>;

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        fetch(req) {
          const url = new URL(req.url);
          if (url.pathname === "/set") {
            return new Response("set", {
              headers: { "Set-Cookie": "napisid=from-server; Path=/" },
            });
          }
          if (url.pathname === "/echo") {
            return new Response(req.headers.get("cookie") ?? "");
          }
          if (url.pathname === "/echo-full") {
            return Response.json({
              url: req.url,
              probe: req.headers.get("x-napi-probe"),
            });
          }
          return new Response("<!doctype html><body>home</body>", {
            headers: { "content-type": "text/html" },
          });
        },
      });
      base = `http://127.0.0.1:${server.port}`;
      browser = await launchForBackend(backend);
      page = await browser.newPage();
      await page.goto(`${base}/`);
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    it("page.request sends browser cookies", async () => {
      const context = page.context();
      await context.addCookies([
        { name: "napictx", value: "browser-side", domain: "127.0.0.1", path: "/", secure: false, httpOnly: false },
      ]);
      const resp = await page.request.get(`${base}/echo`);
      expect(resp.status).toBe(200);
      expect(resp.text()).toContain("napictx=browser-side");
    });

    it("accepts Playwright option shapes: headers object + params scalars", async () => {
      const resp = await page.request.get(`${base}/echo-full`, {
        headers: { "x-napi-probe": "shape-ok" },
        params: { q: "find me", n: 7 },
      });
      expect(resp.status).toBe(200);
      const echoed = resp.json() as any;
      expect(echoed.probe).toBe("shape-ok");
      expect(String(echoed.url)).toContain("q=find");
      expect(String(echoed.url)).toContain("n=7");
    });

    it("Set-Cookie from context.request lands in the browser", async () => {
      const context = page.context();
      const resp = await context.request.get(`${base}/set`);
      expect(resp.status).toBe(200);
      const cookies = await context.cookies();
      const stored = cookies.find((c: any) => c.name === "napisid");
      expect(stored?.value).toBe("from-server");
      // And the page-side document sees it after the API client set it.
      const doc = await page.evaluate("document.cookie");
      expect(String(doc)).toContain("napisid=from-server");
    });
  });
}
