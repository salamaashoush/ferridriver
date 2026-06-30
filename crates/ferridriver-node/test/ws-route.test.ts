// NAPI coverage for page.routeWebSocket / WebSocketRoute (Playwright 1.60).
// Exercises both the fully-mocked path (no server) and the
// connectToServer() passthrough path against a real Bun WebSocket server.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND ? [process.env.FERRIDRIVER_BACKEND] : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`routeWebSocket [${backend}]`, () => {
    let browser: Browser;
    let page: Page;
    let server: ReturnType<typeof Bun.serve>;
    let base: string;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
      server = Bun.serve({
        port: 0,
        fetch(req, srv) {
          if (req.headers.get("upgrade") === "websocket") {
            if (srv.upgrade(req)) return;
            return new Response("upgrade failed", { status: 400 });
          }
          return new Response("<!doctype html><body>ws</body>", { headers: { "content-type": "text/html" } });
        },
        websocket: {
          message(ws, msg) {
            ws.send(`server-echo:${msg}`);
          },
        },
      });
      base = `127.0.0.1:${server.port}`;
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    it("fully-mocked socket: handler.onMessage + send, server never contacted", async () => {
      await page.routeWebSocket(`ws://${base}/mock`, (ws) => {
        ws.onMessage((m: any) => ws.send(`mocked:${m}`));
      });
      await page.goto(`http://${base}/`);
      const got = await page.evaluate(
        (url) =>
          new Promise<string>((resolve, reject) => {
            const ws = new WebSocket(url as string);
            ws.onopen = () => ws.send("hello");
            ws.onmessage = (e) => resolve(e.data as string);
            ws.onerror = () => reject(new Error("ws error"));
          }),
        `ws://${base}/mock`,
      );
      expect(got).toBe("mocked:hello");
    });

    it("connectToServer passthrough: page<->server messages flow", async () => {
      await page.routeWebSocket(`ws://${base}/echo`, (ws) => {
        ws.connectToServer();
      });
      const got = await page.evaluate(
        (url) =>
          new Promise<string>((resolve, reject) => {
            const ws = new WebSocket(url as string);
            ws.onopen = () => ws.send("ping");
            ws.onmessage = (e) => resolve(e.data as string);
            ws.onerror = () => reject(new Error("ws error"));
          }),
        `ws://${base}/echo`,
      );
      expect(got).toBe("server-echo:ping");
    });
  });
}
