// NAPI Rule-9 coverage for `Browser.newContext(options)` —
// `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`.
//
// Each option opens a fresh context, navigates a page, and observes a
// page-side effect produced ONLY when the option took effect. We
// follow the same skip matrix as the Rust integration tests:
// `webkit` skipped because its single-context limitation rejects
// `browser.newContext()` outright.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser } from "../index.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw"];

for (const backend of BACKENDS) {
  describe(`Browser.newContext options [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
    });

    afterAll(async () => {
      await browser.close();
    });

    it("userAgent overrides navigator.userAgent", async () => {
      const ctx = browser.newContext({ userAgent: "FerriUA/Bun (RuleNine)" });
      try {
        const page = await ctx.newPage();
        const ua = await page.evaluate("navigator.userAgent");
        expect(ua).toContain("FerriUA/Bun (RuleNine)");
      } finally {
        await ctx.close();
      }
    });

    it("locale overrides navigator.language", async () => {
      const ctx = browser.newContext({ locale: "fr-FR" });
      try {
        const page = await ctx.newPage();
        const lang = await page.evaluate("navigator.language");
        expect(String(lang)).toMatch(/^fr/);
      } finally {
        await ctx.close();
      }
    });

    it("timezoneId overrides Intl.DateTimeFormat", async () => {
      const ctx = browser.newContext({ timezoneId: "Asia/Tokyo" });
      try {
        const page = await ctx.newPage();
        const tz = await page.evaluate("Intl.DateTimeFormat().resolvedOptions().timeZone");
        expect(tz).toBe("Asia/Tokyo");
      } finally {
        await ctx.close();
      }
    });

    it("colorScheme dark flips matchMedia", async () => {
      const ctx = browser.newContext({ colorScheme: "dark" });
      try {
        const page = await ctx.newPage();
        const dark = await page.evaluate("matchMedia('(prefers-color-scheme: dark)').matches");
        expect(dark).toBe(true);
      } finally {
        await ctx.close();
      }
    });

    it("reducedMotion reduce flips matchMedia", async () => {
      const ctx = browser.newContext({ reducedMotion: "reduce" });
      try {
        const page = await ctx.newPage();
        const reduce = await page.evaluate("matchMedia('(prefers-reduced-motion: reduce)').matches");
        expect(reduce).toBe(true);
      } finally {
        await ctx.close();
      }
    });

    it("viewport sets innerWidth/innerHeight", async () => {
      const ctx = browser.newContext({ viewport: { width: 640, height: 480 } });
      try {
        const page = await ctx.newPage();
        const w = await page.evaluate("window.innerWidth");
        const h = await page.evaluate("window.innerHeight");
        expect(w).toBe(640);
        expect(h).toBe(480);
      } finally {
        await ctx.close();
      }
    });

    it("deviceScaleFactor 2 reflects in devicePixelRatio", async () => {
      const ctx = browser.newContext({
        viewport: { width: 800, height: 600 },
        deviceScaleFactor: 2,
      });
      try {
        const page = await ctx.newPage();
        const dpr = await page.evaluate("window.devicePixelRatio");
        expect(dpr).toBe(2);
      } finally {
        await ctx.close();
      }
    });

    it("hasTouch enables touch capability", async () => {
      const ctx = browser.newContext({
        viewport: { width: 800, height: 600 },
        hasTouch: true,
      });
      try {
        const page = await ctx.newPage();
        // Navigate so the touch emulation override applies to a real
        // document. about:blank may sit in a state where the touch
        // overrides reset between commands; a data: URL gives us a
        // committed document.
        await page.goto("data:text/html,<body></body>");
        const max = await page.evaluate("navigator.maxTouchPoints");
        const onts = await page.evaluate("'ontouchstart' in window");
        // Either signal indicates touch emulation took effect.
        expect((typeof max === "number" && max > 0) || onts === true).toBe(true);
      } finally {
        await ctx.close();
      }
    });

    it("recordVideo wires the video registry into the page", async () => {
      const tmpDir = `/tmp/ferri-bun-bcx-${Math.random().toString(36).slice(2)}`;
      const fs = await import("node:fs/promises");
      await fs.mkdir(tmpDir, { recursive: true });
      try {
        const ctx = browser.newContext({
          recordVideo: { dir: tmpDir, size: { width: 800, height: 450 } },
        });
        const page = await ctx.newPage();
        await page.goto("data:text/html,<h1>rec-1</h1>");
        await page.goto("data:text/html,<h1>rec-2</h1>");
        const video = page.video();
        expect(video).not.toBeNull();
        await ctx.close();
      } finally {
        await fs.rm(tmpDir, { recursive: true, force: true });
      }
    });
  });
}
