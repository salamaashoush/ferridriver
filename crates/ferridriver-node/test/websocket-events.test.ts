// webSocket.on / off / waitForEvent through the binding, with
// Playwright's shapes: { payload } for frames, the socket for 'close'.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] WebSocket events`, () => {
    let browser: Browser;
    let server: ReturnType<typeof Bun.serve>;
    let origin: string;

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        fetch(request, srv) {
          if (srv.upgrade(request)) return undefined;
          return new Response("<p>ws</p>", { headers: { "content-type": "text/html" } });
        },
        websocket: {
          message(ws, message) {
            ws.send(message);
          },
        },
      });
      origin = `http://127.0.0.1:${server.port}`;
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    async function open(page: Page): Promise<any> {
      await page.goto(`${origin}/`);
      const socket = page.waitForEvent("websocket");
      await page.evaluate(`
        window.__ws = new WebSocket(${JSON.stringify(origin.replace("http", "ws"))});
        window.__ws.binaryType = "arraybuffer";
        window.__opened = new Promise(resolve => { window.__ws.onopen = resolve; });
        null`);
      await page.evaluate("window.__opened");
      return await socket;
    }

    it("hands listeners frames and the socket on close", async () => {
      const page = await browser.newPage();
      const ws = await open(page);
      const log: string[] = [];
      const received = (frame: { payload: string | Buffer }) => log.push(`received:${frame.payload}`);
      ws.on("framereceived", received);
      ws.on("framesent", (frame: { payload: string | Buffer }) => log.push(`sent:${frame.payload}`));
      const echo = ws.waitForEvent("framereceived", (frame: { payload: string | Buffer }) => frame.payload === "one");
      await page.evaluate("window.__ws.send('one')");
      expect((await echo).payload).toBe("one");
      ws.off("framereceived", received);

      const binary = ws.waitForEvent("framereceived");
      await page.evaluate("window.__ws.send(new Uint8Array([1, 2, 255]))");
      expect(Buffer.isBuffer((await binary).payload)).toBe(true);

      const closed = ws.waitForEvent("close");
      const pending = ws.waitForEvent("framesent").then(() => "frame", (e: Error) => e.message);
      await page.evaluate("window.__ws.close()");
      expect((await closed).url()).toBe(ws.url());
      expect(await pending).toContain("Socket closed");
      for (let i = 0; i < 50 && !log.includes("received:one"); i++) await Bun.sleep(20);
      expect(log.filter((entry) => entry.endsWith(":one"))).toEqual(["sent:one", "received:one"]);
      await page.close();
    });
  });
}
