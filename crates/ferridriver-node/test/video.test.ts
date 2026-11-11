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
 * Recording is gated to the CDP backends here. Chromium records with
 * its own encoder (`Page.startScreenRecording`, AV1 in MP4); browsers
 * without it fall back to the screencast and ffmpeg.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { extname, join } from "node:path";
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

        const copyPath = join(recordDir, `copy${extname(await video!.path())}`);
        await video!.saveAs(copyPath);
        expect(existsSync(copyPath)).toBe(true);
        expect(statSync(copyPath).size).toBeGreaterThan(0);

        // Delete removes the ORIGINAL file; the copy survives.
        await video!.delete();
        const origPath = await video!.path();
        expect(existsSync(origPath)).toBe(false);
        expect(existsSync(copyPath)).toBe(true);
      },
      20_000,
    );
    /** A page that repaints every animation frame, so a recording has frames to hold. */
    const ANIMATED =
      "data:text/html," +
      encodeURIComponent(
        `<body style="margin:0"><div id=d style="width:100vw;height:100vh"></div>` +
          `<script>let i=0;(function f(){d.style.background=\`hsl(\${i++%360},80%,50%)\`;requestAnimationFrame(f)})()</script>`,
      );

    /**
     * The MP4 brand and the AV1 sample entry Chromium's recorder writes,
     * answering how many frames the file holds. The recorder writes a
     * fragmented MP4, so the frames are the `trun` sample counts of each
     * `moof`; the encoder's bitrate decides the byte size, not the frames.
     */
    function av1Mp4Frames(file: string): number {
      const bytes = readFileSync(file);
      expect(extname(file)).toBe(".mp4");
      expect(bytes.subarray(4, 8).toString()).toBe("ftyp");
      expect(bytes.includes(Buffer.from("av01"))).toBe(true);
      const CONTAINERS = new Set(["moof", "traf"]);
      let frames = 0;
      const walk = (start: number, end: number) => {
        let at = start;
        while (at + 8 <= end) {
          const size = bytes.readUInt32BE(at);
          const type = bytes.subarray(at + 4, at + 8).toString("latin1");
          if (size < 8) break;
          if (type === "trun") frames += bytes.readUInt32BE(at + 12);
          if (CONTAINERS.has(type)) walk(at + 8, at + size);
          at += size;
        }
      };
      walk(0, bytes.length);
      return frames;
    }

    it(
      "Chromium records with its own encoder, into an AV1 MP4 that holds the page's frames",
      async () => {
        const dir = mkdtempSync(join(tmpdir(), "ferridriver-native-"));
        const ctx = browser.newContext({ recordVideo: { dir, size: { width: 640, height: 360 } } });
        const page = await ctx.newPage();
        const video = page.video()!;
        await page.goto(ANIMATED, null);
        await new Promise((r) => setTimeout(r, 1500));
        await page.close(null);
        // The page repaints every animation frame, so a second and a half
        // is dozens of frames at the recorder's 25 a second; a recorder
        // that stalled or stopped early writes a handful.
        expect(av1Mp4Frames(await video.path())).toBeGreaterThanOrEqual(10);
        await ctx.close();
      },
      30_000,
    );

    it(
      "closing the context, not the page, still keeps the recording",
      async () => {
        const dir = mkdtempSync(join(tmpdir(), "ferridriver-native-"));
        const ctx = browser.newContext({ recordVideo: { dir } });
        const page = await ctx.newPage();
        const video = page.video()!;
        await page.goto(ANIMATED, null);
        await new Promise((r) => setTimeout(r, 800));
        await ctx.close();
        expect(av1Mp4Frames(await video.path())).toBeGreaterThanOrEqual(5);
      },
      30_000,
    );

    it(
      "the screencast's frames keep coming while the page records",
      async () => {
        const dir = mkdtempSync(join(tmpdir(), "ferridriver-native-"));
        const ctx = browser.newContext({ recordVideo: { dir } });
        const page = await ctx.newPage();
        const video = page.video()!;
        await page.goto(ANIMATED, null);
        let frames = 0;
        await page.startScreencast(60, 320, 180, () => {
          frames += 1;
        });
        await new Promise((r) => setTimeout(r, 1000));
        await page.stopScreencast();
        await page.close(null);
        expect(frames).toBeGreaterThan(5);
        av1Mp4Frames(await video.path());
        await ctx.close();
      },
      30_000,
    );
  });
}
