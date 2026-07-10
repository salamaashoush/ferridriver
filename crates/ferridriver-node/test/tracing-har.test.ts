// NAPI coverage for context.tracing.startHar() / stopHar() (Playwright
// 1.60).

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { tmpdir } from "os";
import { join } from "path";
import { readFileSync, rmSync } from "fs";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`tracing HAR [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("startHar/stopHar records network into a HAR file", async () => {
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

      } finally {
        server.stop(true);
        rmSync(harPath, { force: true });
      }
    });

    it("startHar to .zip packs har.har plus attached bodies; routeFromHAR replays it offline", async () => {
      const server = Bun.serve({
        port: 0,
        fetch: () =>
          new Response("<!doctype html><body>zipped-har-body</body>", {
            headers: { "content-type": "text/html" },
          }),
      });
      const zipPath = join(tmpdir(), `ferri-har-napi-${backend}-${server.port}.har.zip`);
      try {
        const url = `http://127.0.0.1:${server.port}/page`;
        await page.context().tracing.startHar(zipPath);
        await page.goto(url);
        await page.context().tracing.stopHar();
        server.stop(true);

        const ctx = browser.newContext({});
        try {
          await ctx.routeFromHAR(zipPath, { notFound: "abort" });
          const p = await ctx.newPage();
          await p.goto(url);
          expect(await p.evaluate("document.body.textContent")).toContain("zipped-har-body");
        } finally {
          await ctx.close();
        }
      } finally {
        server.stop(true);
        rmSync(zipPath, { force: true });
      }
    });

    it("routeFromHAR update: true records and writes on context close; RegExp url filters", async () => {
      const server = Bun.serve({
        port: 0,
        fetch: (req) =>
          new Response(`<!doctype html><body>${new URL(req.url).pathname}</body>`, {
            headers: { "content-type": "text/html" },
          }),
      });
      const harPath = join(tmpdir(), `ferri-har-upd-napi-${backend}-${server.port}.har`);
      try {
        const ctx = browser.newContext({});
        const p = await ctx.newPage();
        await ctx.routeFromHAR(harPath, {
          update: true,
          updateContent: "embed",
          url: new RegExp(`127\\.0\\.0\\.1:${server.port}/keep`),
        });
        await p.goto(`http://127.0.0.1:${server.port}/keep`);
        await p.goto(`http://127.0.0.1:${server.port}/drop`);
        await ctx.close();

        const har = JSON.parse(readFileSync(harPath, "utf8"));
        const urls: string[] = har.log.entries.map((e: any) => e.request.url);
        expect(urls.some((u) => u.endsWith("/keep"))).toBe(true);
        expect(urls.some((u) => u.endsWith("/drop"))).toBe(false);
      } finally {
        server.stop(true);
        rmSync(harPath, { force: true });
      }
    });
  });
}
