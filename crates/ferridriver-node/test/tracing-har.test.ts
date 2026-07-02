// NAPI coverage for context.tracing.startHar() / stopHar() (Playwright
// 1.60). tracing.start() (trace .zip recorder) is not implemented and
// must reject.

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

        // trace .zip recorder is not implemented.
        await expect(page.context().tracing.start()).rejects.toThrow();
      } finally {
        server.stop(true);
        rmSync(harPath, { force: true });
      }
    });
  });
}
