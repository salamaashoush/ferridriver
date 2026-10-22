// Playwright's `request.newContext(options)` on the Node binding: the
// options reach the wire, storageState seeds and reports cookies, and
// failOnStatusCode lets 3xx through.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { createServer, connect, type Server } from "node:net";
import { request } from "../index.js";

/**
 * A forwarding HTTP proxy wanting `user:secret`, which resolves `*.test` to
 * loopback (nothing else does) and records the targets it forwarded.
 */
function authenticatedProxy(): Promise<{ port: number; seen: string[]; close: () => void }> {
  const seen: string[] = [];
  const server: Server = createServer((client) => {
    client.once("data", (chunk) => {
      const head = chunk.toString("latin1");
      const auth = /^proxy-authorization: (.*)$/im.exec(head)?.[1]?.trim();
      if (auth !== `Basic ${Buffer.from("user:secret").toString("base64")}`) {
        client.end("HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic\r\nContent-Length: 0\r\n\r\n");
        return;
      }
      const [method, target] = head.split(" ");
      const url = new URL(target ?? "");
      seen.push(`${method} ${target}`);
      const upstream = connect(Number(url.port), url.hostname.endsWith(".test") ? "127.0.0.1" : url.hostname, () => {
        upstream.write(chunk.toString("latin1").replace(target ?? "", url.pathname + url.search), "latin1");
        client.pipe(upstream);
        upstream.pipe(client);
      });
      upstream.on("error", () => client.destroy());
    });
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      resolve({ port, seen, close: () => server.close() });
    });
  });
}

describe("request.newContext", () => {
  let server: ReturnType<typeof Bun.serve>;
  let origin: string;

  beforeAll(() => {
    server = Bun.serve({
      port: 0,
      fetch(req) {
        const path = new URL(req.url).pathname;
        if (path === "/headers") return Response.json(Object.fromEntries(req.headers));
        if (path === "/redirect") return new Response(null, { status: 302, headers: { location: "/headers" } });
        if (path === "/missing") return new Response("no", { status: 404 });
        return new Response("ok");
      },
    });
    origin = `http://127.0.0.1:${server.port}`;
  });

  afterAll(() => server.stop(true));

  it("applies Playwright's options", async () => {
    const api = await request.newContext({
      baseURL: `${origin}/`,
      extraHTTPHeaders: { "x-acme": "one" },
      userAgent: "acme-agent/1.0",
      failOnStatusCode: true,
      maxRedirects: 0,
      storageState: {
        cookies: [{ name: "sid", value: "seeded", domain: "127.0.0.1", path: "/", expires: -1, httpOnly: false, secure: false, sameSite: "Lax" }],
        origins: [],
      },
    });
    const headers = (await (await api.get("headers")).json()) as Record<string, string>;
    expect(headers["x-acme"]).toBe("one");
    expect(headers["user-agent"]).toBe("acme-agent/1.0");
    expect(headers.cookie).toContain("sid=seeded");
    expect((await api.get("redirect")).status()).toBe(302);
    expect(await api.get("missing").then(() => "resolved", (e: Error) => e.message)).toContain("404");
    const state = await api.storageState();
    expect(state.cookies.map((cookie: { name: string }) => cookie.name)).toContain("sid");
  });

  it("sends through a proxy wanting credentials", async () => {
    const proxy = await authenticatedProxy();
    try {
      const api = await request.newContext({
        proxy: { server: `http://127.0.0.1:${proxy.port}`, username: "user", password: "secret" },
      });
      const target = `http://origin.test:${server.port}/headers`;
      const response = await api.get(target);
      expect(response.status()).toBe(200);
      expect(((await response.json()) as Record<string, string>).host).toBe(`origin.test:${server.port}`);
      expect(proxy.seen).toEqual([`GET ${target}`]);

      const refused = await request.newContext({
        proxy: { server: `http://127.0.0.1:${proxy.port}`, username: "user", password: "wrong" },
      });
      expect((await refused.get(target)).status()).toBe(407);
    } finally {
      proxy.close();
    }
  });

  it("refuses what it cannot honour", async () => {
    const refused = await request
      .newContext({ clientCertificates: [] })
      .then(() => "made", (e: Error) => e.message);
    expect(refused).toContain("not supported");
    const misnamed = await request
      .newContext({ proxy: { server: "ftp://proxy.example.com" } })
      .then(() => "made", (e: Error) => e.message);
    expect(misnamed).toContain("proxy");
  });
});
