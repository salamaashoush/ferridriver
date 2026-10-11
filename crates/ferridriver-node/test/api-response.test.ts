// NAPI coverage for the apiResponse surface (Playwright 1.61):
// serverAddr(), the RawHeaders-shaped header accessors, and dispose().

import { describe, it, expect } from "bun:test";
import { HttpClient } from "../index.js";

describe("apiResponse headers", () => {
  it("headers() lowercases and joins, headersArray() stays verbatim", async () => {
    const server = Bun.serve({
      port: 0,
      fetch: () => {
        const headers = new Headers();
        headers.append("Set-Cookie", "a=1");
        headers.append("Set-Cookie", "b=2");
        headers.append("X-Dup", "one");
        headers.append("x-dup", "two");
        return new Response("ok", { headers });
      },
    });
    try {
      const client = HttpClient.create();
      const resp = await client.get(`http://127.0.0.1:${server.port}/h`);

      expect(resp.headers()["set-cookie"]).toBe("a=1\nb=2");
      expect(resp.headers()["x-dup"]).toBe("one, two");
      expect(resp.header("X-DUP")).toBe("one, two");
      expect(resp.header("x-missing")).toBeNull();

      // Only set-cookie reaches the wire as repeated lines — every other
      // repeated name is comma-joined by the sender before it is sent.
      const cookies = resp.headersArray().filter((h) => h.name.toLowerCase() === "set-cookie");
      expect(cookies.length).toBe(2);
      expect(cookies.map((h) => h.value)).toEqual(["a=1", "b=2"]);
    } finally {
      server.stop(true);
    }
  });

  it("dispose() releases the body and keeps the metadata", async () => {
    const server = Bun.serve({ port: 0, fetch: () => new Response("payload") });
    try {
      const client = HttpClient.create();
      const resp = await client.get(`http://127.0.0.1:${server.port}/d`);
      expect(resp.text()).toBe("payload");

      resp.dispose();

      expect(() => resp.body()).toThrow(/disposed/);
      expect(() => resp.text()).toThrow(/disposed/);
      expect(resp.status).toBe(200);
      expect(resp.headers()["content-type"]).toContain("text/plain");
    } finally {
      server.stop(true);
    }
  });
});

describe("apiResponse.serverAddr", () => {
  it("reports the resolved peer address", async () => {
    const server = Bun.serve({ port: 0, fetch: () => new Response("ok") });
    try {
      const client = HttpClient.create();
      const resp = await client.get(`http://127.0.0.1:${server.port}/api`);
      expect(resp.status).toBe(200);
      const addr = resp.serverAddr();
      expect(addr).not.toBeNull();
      expect(addr!.ipAddress).toBe("127.0.0.1");
      expect(addr!.port).toBe(server.port);
    } finally {
      server.stop(true);
    }
  });
});

describe("apiResponse.timing and securityDetails", () => {
  it("reports lookup and connect for a new connection, and neither for a reused one", async () => {
    const server = Bun.serve({ port: 0, fetch: () => new Response("ok") });
    try {
      const client = HttpClient.create();
      const before = Date.now();
      const first = await client.get(`http://localhost:${server.port}/t`);
      const t = first.timing();
      expect(t.startTime).toBeGreaterThanOrEqual(before - 1);
      expect(t.startTime).toBeLessThanOrEqual(Date.now());
      expect(t.domainLookupStart).toBe(0);
      expect(t.domainLookupEnd).toBeGreaterThanOrEqual(0);
      expect(t.connectStart).toBe(t.domainLookupEnd);
      expect(t.connectEnd).toBeGreaterThanOrEqual(t.connectStart);
      expect(t.secureConnectionStart).toBe(-1);
      expect(t.requestStart).toBe(t.connectEnd);
      expect(t.responseStart).toBeGreaterThanOrEqual(t.requestStart);
      expect(t.responseEnd).toBeGreaterThanOrEqual(t.responseStart);
      expect(first.securityDetails()).toBeNull();

      const again = (await client.get(`http://localhost:${server.port}/t`)).timing();
      expect([again.domainLookupStart, again.domainLookupEnd, again.connectStart, again.connectEnd]).toEqual([
        -1, -1, -1, -1,
      ]);
      expect(again.requestStart).toBe(0);
      expect(again.responseStart).toBeGreaterThan(0);
    } finally {
      server.stop(true);
    }
  });
});

describe("request context cookies", () => {
  it("reads, fills and empties the client's own jar", async () => {
    const server = Bun.serve({
      port: 0,
      fetch: (req) =>
        new URL(req.url).pathname === "/set"
          ? new Response("set", { headers: { "Set-Cookie": "sid=abc; Path=/" } })
          : new Response(req.headers.get("cookie") ?? ""),
    });
    const origin = `http://127.0.0.1:${server.port}`;
    const blank = { domain: "", path: "", secure: false, httpOnly: false };
    try {
      const client = HttpClient.create();
      await client.addCookies([{ ...blank, name: "token", value: "one", url: `${origin}/app/page` }]);
      await client.get(`${origin}/set`);
      const names = async (urls?: string | string[]) => (await client.cookies(urls)).map((c) => c.name).sort();
      expect(await names()).toEqual(["sid", "token"]);
      expect((await client.cookies()).find((c) => c.name === "token")).toMatchObject({
        domain: "127.0.0.1",
        path: "/app/",
        expires: -1,
        sameSite: "Lax",
      });
      expect(await names(`${origin}/other`)).toEqual(["sid"]);
      expect(await names([`${origin}/app/x`, "http://example.com/"])).toEqual(["sid", "token"]);
      expect((await client.get(`${origin}/app/echo`)).text()).toBe("token=one; sid=abc");

      await client.clearCookies({ name: /^s/ });
      expect(await names()).toEqual(["token"]);
      await client.clearCookies();
      expect(await client.cookies()).toEqual([]);
      expect((await client.get(`${origin}/app/echo`)).text()).toBe("");

      await expect(client.addCookies([{ ...blank, name: "a", value: "1" }])).rejects.toThrow(
        /Cookie should have a url or a domain\/path pair/,
      );
    } finally {
      server.stop(true);
    }
  });
});
