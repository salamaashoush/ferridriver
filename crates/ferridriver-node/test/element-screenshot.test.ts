/**
 * `locator.screenshot(options)` and `elementHandle.screenshot(options)`
 * take Playwright's whole option bag, and each option reaches the pixels.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { decodePngRgba, launchForBackend, pixelAt } from "./_helpers.js";

const FIXTURE =
  "<style>html,body{margin:0;background:#fff}</style>" +
  '<div id="box" style="margin:10px;width:60px;height:40px;background:#f00">' +
  '<div id="inner" style="width:20px;height:20px;background:#00f"></div></div>';

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", "webkit"];

for (const backend of BACKENDS) {
  describe(`[${backend}] element screenshot options`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      const context = await browser.newContext({ viewport: { width: 300, height: 200 }, deviceScaleFactor: 2 });
      page = await context.newPage();
      await page.setContent(FIXTURE);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("locator.screenshot honours scale, mask and style", async () => {
      const box = page.locator("#box");
      const shot = decodePngRgba(
        await box.screenshot({ scale: "css", mask: [page.locator("#inner")], maskColor: "#ff00ff", style: "#box{background:#0f0 !important}" }),
      );
      expect([shot.width, shot.height]).toEqual([60, 40]);
      expect(pixelAt(shot, 10, 10)).toEqual([255, 0, 255]);
      expect(pixelAt(shot, 50, 10)).toEqual([0, 255, 0]);
      const device = decodePngRgba(await box.screenshot());
      expect([device.width, device.height]).toEqual([120, 80]);
    });

    it("elementHandle.screenshot takes the same bag and validates quality", async () => {
      const handle = (await page.$("#box"))!;
      const jpeg = await handle.screenshot({ type: "jpeg", quality: 40 });
      expect([jpeg[0], jpeg[1]]).toEqual([0xff, 0xd8]);
      const error = await handle.screenshot({ type: "png", quality: 40 }).catch((e: Error) => String(e));
      expect(error).toContain("options.quality is unsupported for the png screenshots");
    });
  });
}
