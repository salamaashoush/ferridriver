// Playwright's names on the binding: page/context setExtraHTTPHeaders
// reach the server, and connectOverCDP attaches to a running Chromium.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { chromium, type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] setExtraHTTPHeaders`, () => {
    let browser: Browser;
    let server: ReturnType<typeof Bun.serve>;
    let origin: string;
    const seen: Record<string, string | null> = {};

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        fetch(request) {
          seen[new URL(request.url).pathname] = request.headers.get("x-acme");
          return new Response("<p>ok</p>", { headers: { "content-type": "text/html" } });
        },
      });
      origin = `http://127.0.0.1:${server.port}`;
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    it("sends the page and context headers", async () => {
      const context = await browser.newContext();
      await context.setExtraHTTPHeaders({ "x-acme": "context" });
      const page = await context.newPage();
      await page.goto(`${origin}/context`);
      expect(seen["/context"]).toBe("context");
      await page.setExtraHTTPHeaders({ "x-acme": "page" });
      await page.goto(`${origin}/page`);
      expect(seen["/page"]).toBe("page");
      await context.close();
    });
  });
}

describe("connectOverCDP", () => {
  it("attaches to a Chromium started with a debugging port", async () => {
    const owner = await chromium({ transport: "ws" }).launch();
    try {
      const session = await owner.newBrowserCDPSession();
      const line = (await session.send("Browser.getBrowserCommandLine")) as { arguments: string[] };
      const profile = line.arguments.find((arg) => arg.startsWith("--user-data-dir="))!.slice("--user-data-dir=".length);
      const [port] = (await Bun.file(`${profile}/DevToolsActivePort`).text()).split("\n");
      await session.detach();
      const attached = await chromium().connectOverCDP(`http://127.0.0.1:${port}`);
      expect(attached.isConnected()).toBe(true);
      await attached.close();
    } finally {
      await owner.close();
    }
  });
});
