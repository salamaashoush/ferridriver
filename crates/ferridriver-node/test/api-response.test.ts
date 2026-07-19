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
