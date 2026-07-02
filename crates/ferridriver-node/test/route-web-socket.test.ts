// NAPI coverage for page.routeWebSocket / context.routeWebSocket /
// WebSocketRoute (Playwright 1.60). Exercises the fully-mocked path (no
// server), the connectToServer() passthrough path against a real Bun
// WebSocket server, the context-level fan-out, and the two-argument
// (code, reason) onClose handler shape.

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

    it("context.routeWebSocket: fully-mocked echo at the context level", async () => {
      await page.context().routeWebSocket(`ws://${base}/ctxmock`, (ws) => {
        ws.onMessage((m: any) => ws.send(`ctx:${m}`));
      });
      await page.goto(`http://${base}/`);
      const got = await page.evaluate(
        (url) =>
          new Promise<string>((resolve, reject) => {
            const ws = new WebSocket(url as string);
            ws.onopen = () => ws.send("hi");
            ws.onmessage = (e) => resolve(e.data as string);
            ws.onerror = () => reject(new Error("ws error"));
          }),
        `ws://${base}/ctxmock`,
      );
      expect(got).toBe("ctx:hi");
    });

    it("onClose handler receives (code, reason) as two positional args", async () => {
      const received: Array<{ code?: number; reason?: string; codeType: string; reasonType: string }> = [];
      await page.routeWebSocket(`ws://${base}/closer`, (ws) => {
        ws.onClose((code, reason) => {
          received.push({ code, reason, codeType: typeof code, reasonType: typeof reason });
        });
      });
      await page.goto(`http://${base}/`);
      // Open the socket and stash it, let the open handshake settle, then
      // close it from a separate evaluate so the close frame is dispatched
      // after the driver has fully wired the route.
      await page.evaluate(
        (url) =>
          new Promise<void>((resolve) => {
            const ws = new WebSocket(url as string);
            (globalThis as any).__closer = ws;
            ws.onopen = () => resolve();
            ws.onerror = () => resolve();
          }),
        `ws://${base}/closer`,
      );
      await new Promise((r) => setTimeout(r, 200));
      await page.evaluate(() => (globalThis as any).__closer.close(4001, "bye"));
      for (let i = 0; i < 60 && received.length === 0; i++) {
        await new Promise((r) => setTimeout(r, 20));
      }
      expect(received.length).toBeGreaterThan(0);
      expect(received[0].code).toBe(4001);
      expect(received[0].reason).toBe("bye");
      // Verify the two-arg positional shape, not a single { code, reason } object.
      expect(received[0].codeType).toBe("number");
      expect(received[0].reasonType).toBe("string");
    });
  });
}
