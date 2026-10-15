// route.fetch() and Playwright-shaped route.continue() overrides through
// the Node binding.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] route.fetch`, () => {
    let browser: Browser;
    let page: Page;
    let server: ReturnType<typeof Bun.serve>;
    let origin: string;

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        async fetch(request) {
          const url = new URL(request.url);
          if (url.pathname === "/page") return new Response("<p>page</p>", { headers: { "content-type": "text/html" } });
          return Response.json({
            path: url.pathname,
            method: request.method,
            header: request.headers.get("x-route"),
            body: await request.text(),
          });
        },
      });
      origin = `http://127.0.0.1:${server.port}`;
      browser = await launchForBackend(backend);
      page = await browser.newPage();
      await page.goto(`${origin}/page`);
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    it("fetches with overrides and fulfils with the response", async () => {
      await page.route("**/intercepted", async (route) => {
        const response = await route.fetch({
          url: `${origin}/echo`,
          method: "POST",
          headers: { "x-route": "fetched" },
          postData: "sashoush",
        });
        const echoed = await response.json();
        await route.fulfill({ response, body: JSON.stringify({ ...echoed, patched: true }) });
      });
      const got = JSON.parse(await page.evaluate("fetch('/intercepted').then(r => r.text())"));
      expect(got).toEqual({ path: "/echo", method: "POST", header: "fetched", body: "sashoush", patched: true });
      await page.unroute("**/intercepted");
    });

    it("continue takes headers as a record and postData", async () => {
      await page.route("**/continued", (route) =>
        route.continue({ headers: { "x-route": "continued" }, method: "POST", postData: "body" }),
      );
      const got = JSON.parse(await page.evaluate("fetch('/continued').then(r => r.text())"));
      expect(got).toEqual({ path: "/continued", method: "POST", header: "continued", body: "body" });
      await page.unroute("**/continued");
    });
  });
}
