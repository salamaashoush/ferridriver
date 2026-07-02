// NAPI coverage for apiResponse.serverAddr() (Playwright 1.61).

import { describe, it, expect } from "bun:test";
import { HttpClient } from "../index.js";

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
