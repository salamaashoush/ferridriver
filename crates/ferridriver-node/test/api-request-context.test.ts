// Playwright's `request.newContext(options)` on the Node binding: the
// options reach the wire, storageState seeds and reports cookies, and
// failOnStatusCode lets 3xx through.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { request } from "../index.js";

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

  it("refuses what it cannot honour", async () => {
    const refused = await request
      .newContext({ proxy: { server: "http://proxy.example.com" } })
      .then(() => "made", (e: Error) => e.message);
    expect(refused).toContain("not supported");
  });
});
