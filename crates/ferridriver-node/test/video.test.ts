/**
 * NAPI parity tests for the Video lifecycle handle.
 *
 * Playwright public API shapes (verified against
 * `/tmp/playwright/packages/playwright-core/types/types.d.ts:21621`):
 *
 * - `page.video(): null | Video` — null when `recordVideo` wasn't set
 *   on the context (`types.d.ts:4756`).
 * - `video.path(): Promise<string>` — resolves once the page closes
 *   and the recording file is finalised.
 * - `video.saveAs(path): Promise<void>` — copies the finalised file.
 * - `video.delete(): Promise<void>` — removes the finalised file.
 *
 * Recording requires a working CDP `Page.startScreencast` path, so
 * these tests are gated to the CDP backends. BiDi's polyfill is
 * covered by the Rust integration suite; WebKit's typed `Unsupported`
 * is covered there too.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type Browser, type BrowserContext } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

for (const backend of BACKENDS) {
  describe(`[${backend}] Video as first-class handle (§2.14)`, () => {
    let browser: Browser;
    let recordDir: string;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      recordDir = mkdtempSync(join(tmpdir(), "ferridriver-video-"));
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("page.video() returns null when recordVideo wasn't set", async () => {
      const ctx = browser.defaultContext();
      const page = await ctx.newPage();
      expect(page.video()).toBeNull();
      await page.close(null);
    });

    it(
      "records, path() resolves to an existing non-empty file",
      async () => {
        const ctx = browser.defaultContext();
        await ctx.setRecordVideo({ dir: recordDir });
        const page = await ctx.newPage();
        const video = page.video();
        expect(video).not.toBeNull();
        await page.goto("data:text/html,<h1>rec</h1>", null);
        // Give the encoder a few frames of content before closing.
        await new Promise((r) => setTimeout(r, 500));
        await page.close(null);
        const filePath = await video!.path();
        expect(typeof filePath).toBe("string");
        expect(filePath).toContain(recordDir);
        expect(existsSync(filePath)).toBe(true);
        expect(statSync(filePath).size).toBeGreaterThan(0);
      },
      20_000,
    );

    it(
      "saveAs copies, delete removes the file",
      async () => {
        const ctx: BrowserContext = browser.defaultContext();
        await ctx.setRecordVideo({ dir: recordDir });
        const page = await ctx.newPage();
        const video = page.video();
        expect(video).not.toBeNull();
        await page.goto("data:text/html,<h1>save</h1>", null);
        await new Promise((r) => setTimeout(r, 500));
        await page.close(null);

        const copyPath = join(recordDir, "copy.webm");
        await video!.saveAs(copyPath);
        expect(existsSync(copyPath)).toBe(true);
        expect(statSync(copyPath).size).toBeGreaterThan(0);

        // Delete removes the ORIGINAL file; saveAs copied to `copy.webm`
        // which survives.
        await video!.delete();
        const origPath = await video!.path();
        expect(existsSync(origPath)).toBe(false);
        expect(existsSync(copyPath)).toBe(true);
      },
      20_000,
    );
  });
}
