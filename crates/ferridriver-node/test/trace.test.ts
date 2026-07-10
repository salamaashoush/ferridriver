// NAPI coverage for context.tracing.start()/stop() — Playwright trace
// format VERSION 8.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { tmpdir } from "os";
import { join } from "path";
import { rmSync } from "fs";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`tracing [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("records a v8 trace.zip with actions and screenshots", async () => {
      const page = await browser.newPage();
      const ctx = page.context();
      const tracePath = join(tmpdir(), `ferri-trace-napi-${backend}-${Date.now()}.zip`);
      try {
        await ctx.tracing.start({ title: "napi trace", screenshots: true });
        await page.goto("data:text/html,<body><button id=b>Go</button></body>");
        await page.locator("#b").click();
        await ctx.tracing.stop({ path: tracePath });

        const zipBytes = await Bun.file(tracePath).bytes();
        expect(zipBytes.length).toBeGreaterThan(0);
        // Zip local-file header magic.
        expect(zipBytes[0]).toBe(0x50);
        expect(zipBytes[1]).toBe(0x4b);

        // Bun has no zip reader in stdlib; shell out to unzip -p for the
        // trace log and validate the loader-critical first line.
        const proc = Bun.spawnSync(["unzip", "-p", tracePath, "trace.trace"]);
        const trace = new TextDecoder().decode(proc.stdout);
        const lines = trace.split("\n").filter((l) => l.trim().length > 0);
        const first = JSON.parse(lines[0]);
        expect(first.type).toBe("context-options");
        expect(first.version).toBe(8);
        const actions = lines.map((l) => JSON.parse(l)).filter((e) => e.type === "action");
        expect(actions.some((a) => a.method === "goto")).toBe(true);
        expect(actions.some((a) => a.method === "click")).toBe(true);

        await expect(ctx.tracing.stop()).rejects.toThrow(/Must start tracing/);
      } finally {
        rmSync(tracePath, { force: true });
      }
    });
  });
}
