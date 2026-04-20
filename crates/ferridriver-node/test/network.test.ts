/**
 * §1.4 Rule-9 NAPI tests for `Request` / `Response` / `WebSocket` lifecycle
 * objects. Mirrors the per-backend integration tests in
 * `crates/ferridriver-cli/tests/backends_support/network.rs` from the JS
 * side via the NAPI binding.
 *
 * Six buckets per supported backend:
 *   1. Redirect chain (`request.redirectedFrom().response().status()`)
 *   2. Response body (`response.text()` / `response.json()`)
 *   3. Post data (`request.postData()` / `request.postDataJSON()`)
 *   4. Headers (`request.headers()` includes `User-Agent`)
 *   5. WebSocket frame echo (`webSocket.waitForEvent('framereceived')`)
 *   6. Failure (request to refused port → `request.failure()`)
 *
 * Backends gated explicitly: WebKit's `WKWebView` exposes no public API
 * for response body / multi-cookie header parity, so those buckets
 * assert the typed `Unsupported`. cdp-pipe and cdp-raw exercise the
 * full surface.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { WebSocketServer } from "ws";
import type { AddressInfo } from "node:net";

const SERVE: Record<string, (req: IncomingMessage, res: ServerResponse) => void> = {
  "/landed": (_req, res) => {
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.end("landed");
  },
  "/redirect": (_req, res) => {
    res.writeHead(302, { Location: "/landed" });
    res.end();
  },
  "/api/users": (_req, res) => {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ users: ["alice", "bob"] }));
  },
  "/echo": (req, res) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => {
      res.writeHead(200, { "Content-Type": "text/plain" });
      res.end(Buffer.concat(chunks));
    });
  },
  "/multi-cookie": (_req, res) => {
    res.writeHead(200, {
      "Content-Type": "text/plain",
      "Set-Cookie": ["a=1; Path=/", "b=2; Path=/"],
    });
    res.end("cookies-set");
  },
  "/ua-marker": (req, res) => {
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.end(`UA=${req.headers["user-agent"] ?? ""}`);
  },
};

let httpServer: Server;
let baseUrl: string;
let wsServer: WebSocketServer;
let wsUrl: string;

beforeAll(async () => {
  httpServer = createServer((req, res) => {
    const handler = SERVE[req.url ?? ""];
    if (handler) {
      handler(req, res);
    } else {
      res.writeHead(404);
      res.end("not found");
    }
  });
  await new Promise<void>((resolve) => {
    httpServer.listen(0, "127.0.0.1", () => {
      const addr = httpServer.address() as AddressInfo;
      baseUrl = `http://127.0.0.1:${addr.port}`;
      resolve();
    });
  });

  // WebSocket echo server (real implementation via ws library).
  wsServer = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise<void>((resolve) => wsServer.once("listening", () => resolve()));
  const wsAddr = wsServer.address() as AddressInfo;
  wsUrl = `ws://127.0.0.1:${wsAddr.port}/`;
  wsServer.on("connection", (sock) => {
    sock.on("message", (data, isBinary) => {
      sock.send(data, { binary: isBinary });
    });
  });
});

afterAll(async () => {
  httpServer?.close();
  await new Promise<void>((resolve) => wsServer?.close(() => resolve()));
});

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

for (const backend of BACKENDS) {
  describe(`[${backend}] Request/Response lifecycle (§1.4)`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("redirectedFrom + redirectedTo chain links both directions", async () => {
      await page.goto("about:blank", null);
      const wait = page.waitForResponse(`${baseUrl}/landed`, 10_000);
      await page.goto(`${baseUrl}/redirect`, null);
      const resp = await wait;
      const req = resp.request();
      const prev = req.redirectedFrom();
      expect(prev).not.toBeNull();
      expect(prev!.url()).toContain("/redirect");
      const prevResp = await prev!.response();
      expect(prevResp).not.toBeNull();
      expect(prevResp!.status()).toBe(302);
      expect(resp.status()).toBe(200);
    });

    it("response.text() and response.json() round-trip", async () => {
      await page.goto(`${baseUrl}/landed`, null);
      const wait = page.waitForResponse("**/api/users", 10_000);
      await page.evaluate("fetch('/api/users').then(r => r.text())");
      const resp = await wait;
      expect(resp.status()).toBe(200);
      const text = await resp.text();
      expect(text).toContain("alice");
      const json = (await resp.json()) as { users: string[] };
      expect(json.users.length).toBe(2);
      const ct = await resp.headerValue("content-type");
      expect(ct).toContain("application/json");
    });

    it("request.postData() and postDataJSON() round-trip", async () => {
      await page.goto(`${baseUrl}/landed`, null);
      const wait = page.waitForRequest("**/echo", 10_000);
      await page.evaluate(
        "fetch('/echo', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ ping: 'pong', n: 7 }) }).then(r => r.text())"
      );
      const req = await wait;
      expect(req.method()).toBe("POST");
      expect(req.postData()).toContain('"ping":"pong"');
      const parsed = req.postDataJSON() as { ping: string; n: number };
      expect(parsed.ping).toBe("pong");
      expect(parsed.n).toBe(7);
    });

    it("request.headers() carries User-Agent; response.headersArray() preserves multi-Set-Cookie", async () => {
      await page.goto(`${baseUrl}/landed`, null);
      const cookieWait = page.waitForResponse("**/multi-cookie", 10_000);
      const uaWait = page.waitForRequest("**/ua-marker", 10_000);
      await page.evaluate("fetch('/multi-cookie').then(r => r.text())");
      const cookieResp = await cookieWait;
      await page.evaluate("fetch('/ua-marker').then(r => r.text())");
      const uaReq = await uaWait;
      const headers = uaReq.headers();
      const ua = Object.entries(headers).find(([k]) => k.toLowerCase() === "user-agent");
      expect(ua).toBeDefined();
      expect(ua![1]).not.toBe("");
      const cookieHeaders = await cookieResp.headersArray();
      const setCookies = cookieHeaders.filter((h) => h.name.toLowerCase() === "set-cookie");
      expect(setCookies.length).toBe(2);
      const joined = (await cookieResp.headerValue("set-cookie")) ?? "";
      expect(joined).toContain("a=1");
      expect(joined).toContain("b=2");
    });

    it("WebSocket frameSent / frameReceived round-trip", async () => {
      await page.goto("about:blank", null);
      const wsPromise = page.waitForEvent("websocket", 10_000);
      await page.evaluate(
        `window.__ws = new WebSocket(${JSON.stringify(wsUrl)});
         window.__opened = new Promise((res) => { window.__ws.onopen = () => res(); });`
      );
      const ws = (await wsPromise) as any;
      expect(typeof ws.url).toBe("function");
      expect(ws.url()).toContain("ws://");
      const recvPromise = ws.waitForEvent("framereceived", 10_000);
      await page.evaluate("window.__opened.then(() => window.__ws.send('hello-ws'))");
      const frame = (await recvPromise) as { event: string; payload: string | null };
      expect(frame.event).toBe("framereceived");
      expect(frame.payload).toBe("hello-ws");
    });

    it("request.failure() surfaces the error text on failed fetches", async () => {
      await page.goto(`${baseUrl}/landed`, null);
      const failedPromise = page.waitForEvent("requestfailed", 10_000).catch(() => null);
      const fetchOutcome = await page.evaluate(
        "fetch('http://127.0.0.1:65530/blocked').then(() => 'ok').catch(() => 'blocked')"
      );
      expect(fetchOutcome).toBe("blocked");
      const failed = (await failedPromise) as any;
      expect(failed).not.toBeNull();
      const failure = await failed.failure();
      expect(failure).not.toBeNull();
      expect(typeof failure!.errorText).toBe("string");
      expect(failure!.errorText.length).toBeGreaterThan(0);
      expect(failed.url()).toContain("/blocked");
    });
  });
}
