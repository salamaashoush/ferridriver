// page.requests() and page.on('crash') through the binding.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { chromium, type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] page.requests`, () => {
    let browser: Browser;
    let server: ReturnType<typeof Bun.serve>;
    let origin: string;

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        fetch: () => new Response("<p>ok</p>", { headers: { "content-type": "text/html" } }),
      });
      origin = `http://127.0.0.1:${server.port}`;
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    it("lists the page's requests, oldest first", async () => {
      const page = await browser.newPage();
      await page.goto(`${origin}/one`);
      await page.goto(`${origin}/two`);
      let urls: string[] = [];
      for (let i = 0; i < 100 && !urls.includes(`${origin}/two`); i++) {
        urls = (await page.requests()).map((request: any) => request.url());
        await Bun.sleep(20);
      }
      expect(urls.indexOf(`${origin}/one`)).toBeGreaterThanOrEqual(0);
      expect(urls.indexOf(`${origin}/one`)).toBeLessThan(urls.indexOf(`${origin}/two`));
      await page.close();
    });
  });
}

describe("page.on('crash')", () => {
  it("fires when the renderer crashes", async () => {
    const browser = await chromium().launch();
    try {
      const page = await browser.newPage();
      const crashed = new Promise<boolean>((resolve) => page.on("crash", () => resolve(true)));
      page.goto("chrome://crash").catch(() => null);
      expect(await crashed).toBe(true);
      expect(page.isClosed()).toBe(false);
    } finally {
      await browser.close();
    }
  });
});
