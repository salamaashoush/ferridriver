// browserContext.grantPermissions(permissions, { origin }) through the
// binding: the grant reaches only that origin, and grants add up.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

const state = (page: Page, name: string) =>
  page.evaluate(`navigator.permissions.query({ name: ${JSON.stringify(name)} }).then(s => s.state)`);

for (const backend of BACKENDS) {
  describe(`[${backend}] grantPermissions`, () => {
    let browser: Browser;
    let server: ReturnType<typeof Bun.serve>;
    let port: number;

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        fetch: () => new Response("<p>ok</p>", { headers: { "content-type": "text/html" } }),
      });
      port = server.port;
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    it("grants for the origin it names and adds to earlier grants", async () => {
      const context = await browser.newContext();
      const page = await context.newPage();
      await page.goto(`http://localhost:${port}/`);
      const elsewhere = await state(page, "geolocation");
      await context.grantPermissions(["geolocation"], { origin: `http://127.0.0.1:${port}` });
      expect(await state(page, "geolocation")).toBe(elsewhere);
      await page.goto(`http://127.0.0.1:${port}/`);
      expect(await state(page, "geolocation")).toBe("granted");
      await context.grantPermissions(["notifications"], { origin: `http://127.0.0.1:${port}` });
      expect(await state(page, "geolocation")).toBe("granted");
      expect(await state(page, "notifications")).toBe("granted");
      await context.close();
    });
  });
}
