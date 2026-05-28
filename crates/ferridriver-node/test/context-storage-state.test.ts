// NAPI Rule-9 coverage for `BrowserContext.storageState(options?)` —
// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:460`.
//
// We set a cookie + a localStorage entry on a real http origin, then call
// `context.storageState()` and assert BOTH are present in the exported state,
// with Playwright's exact `{ cookies, origins:[{origin, localStorage:[{name,
// value}]}] }` shape. We also assert `{ path }` writes the same JSON to disk.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readFileSync, rmSync } from "node:fs";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

let testServer: Server;
let testUrl = "";
let testHost = "";

const FIXTURE = `<!DOCTYPE html><html><head><title>storage</title></head><body><h1>storage</h1></body></html>`;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(FIXTURE);
  });
  await new Promise<void>((resolve) => {
    testServer.listen(0, "127.0.0.1", () => {
      const addr = testServer.address() as any;
      testUrl = `http://127.0.0.1:${addr.port}`;
      testHost = `127.0.0.1`;
      resolve();
    });
  });
});

afterAll(() => {
  testServer?.close();
});

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw"];

for (const backend of BACKENDS) {
  describe(`BrowserContext.storageState [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("exports cookies + per-origin localStorage", async () => {
      const ctx = browser.newContext();
      try {
        const page = await ctx.newPage();
        await page.goto(testUrl, null);
        await ctx.addCookies([
          { name: "sid", value: "abc", domain: testHost, path: "/", secure: false, httpOnly: false },
        ]);
        await page.evaluate("localStorage.setItem('token', 't1')");

        const state = await ctx.storageState();

        const sid = state.cookies.find((c) => c.name === "sid");
        expect(sid).toBeDefined();
        expect(sid!.value).toBe("abc");

        const origin = state.origins.find((o) => o.origin === testUrl);
        expect(origin).toBeDefined();
        const token = origin!.localStorage.find((kv) => kv.name === "token");
        expect(token).toBeDefined();
        expect(token!.value).toBe("t1");
      } finally {
        await ctx.close();
      }
    });

    it("writes the same JSON to { path }", async () => {
      const ctx = browser.newContext();
      const out = join(tmpdir(), `ferri-storage-${backend}-${Date.now()}.json`);
      try {
        const page = await ctx.newPage();
        await page.goto(testUrl, null);
        await page.evaluate("localStorage.setItem('persisted', 'yes')");

        const state = await ctx.storageState({ path: out });
        const onDisk = JSON.parse(readFileSync(out, "utf8"));

        expect(onDisk).toEqual(state);
        const origin = onDisk.origins.find((o: any) => o.origin === testUrl);
        expect(origin).toBeDefined();
        expect(origin.localStorage.find((kv: any) => kv.name === "persisted")?.value).toBe("yes");
      } finally {
        rmSync(out, { force: true });
        await ctx.close();
      }
    });

    it("omits origins with empty localStorage", async () => {
      const ctx = browser.newContext();
      try {
        const page = await ctx.newPage();
        await page.goto(testUrl, null);
        // No localStorage set on this fresh origin.
        const state = await ctx.storageState();
        expect(state.origins.find((o) => o.origin === testUrl)).toBeUndefined();
      } finally {
        await ctx.close();
      }
    });
  });
}
