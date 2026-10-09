/**
 * Page.screenshot({ mask: Locator[] }) parity with Playwright.
 *
 * Issue #10: `mask` previously took selector strings (a wire-shape leak).
 * Playwright's signature is `mask?: Locator[]`. After the fix the NAPI
 * binding accepts real `Locator` instances (lowered to their `.selector`
 * via the prototype-chain read) and the generated `.d.ts` shows
 * `mask?: Array<Locator>`.
 *
 * Rule 9 observable effect: a masked element's pixels are overpainted
 * with `maskColor`. We capture once with no mask and once masking a
 * solid green box with magenta, then decode the PNG and assert the
 * masked pixel changed from green to the exact mask color. Passing a
 * Locator that matches nothing leaves the capture unchanged.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { decodePngRgba, launchForBackend, pixelAt } from "./_helpers.js";
import { createServer, type Server } from "node:http";

let testServer: Server;
let testUrl: string;

// A single fixed-position green box at (0,0) sized 100x100 on a white
// page. Masking it paints the top-left region with the mask color.
const FIXTURE = `<!DOCTYPE html>
<html>
  <head><title>mask fixture</title>
  <style>
    html, body { margin: 0; padding: 0; background: #ffffff; }
    #box { position: fixed; left: 0; top: 0; width: 100px; height: 100px; background: #00ff00; }
  </style>
  </head>
  <body><div id="box"></div></body>
</html>`;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(FIXTURE);
  });
  await new Promise<void>((resolve) => {
    testServer.listen(0, "127.0.0.1", () => {
      const addr = testServer.address() as any;
      testUrl = `http://127.0.0.1:${addr.port}`;
      resolve();
    });
  });
});

afterAll(() => {
  testServer?.close();
});

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (() => {
      const b = ["cdp-pipe", "cdp-raw"];
      if (process.platform === "darwin") b.push("webkit");
      return b;
    })();

for (const backend of BACKENDS) {
  describe(`[${backend}] Page.screenshot mask: Locator[]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("masks a real Locator, overpainting its pixels with maskColor", async () => {
      // Sample point well inside the 100x100 box, in CSS pixels. Use
      // scale:'css' so device-pixel ratio doesn't shift coordinates.
      const sx = 20;
      const sy = 20;

      const plain = await page.screenshot({ type: "png", scale: "css" });
      const plainImg = decodePngRgba(plain);
      const plainPx = pixelAt(plainImg, sx, sy);
      // Sanity: the box is green before masking.
      expect(plainPx[0]).toBeLessThan(64);
      expect(plainPx[1]).toBeGreaterThan(192);
      expect(plainPx[2]).toBeLessThan(64);

      // Mask the box with a custom, unmistakable color.
      const masked = await page.screenshot({
        type: "png",
        scale: "css",
        mask: [page.locator("#box")],
        maskColor: "#ff00ff",
      });
      const maskedImg = decodePngRgba(masked);
      const maskedPx = pixelAt(maskedImg, sx, sy);
      // The masked pixel must now be magenta, not green.
      expect(maskedPx[0]).toBeGreaterThan(192);
      expect(maskedPx[1]).toBeLessThan(64);
      expect(maskedPx[2]).toBeGreaterThan(192);
    });

    it("masking a Locator that matches nothing leaves the capture unchanged", async () => {
      const plain = await page.screenshot({ type: "png", scale: "css" });
      const withEmptyMask = await page.screenshot({
        type: "png",
        scale: "css",
        mask: [page.locator("#does-not-exist")],
        maskColor: "#ff00ff",
      });
      // No matching element -> no overlay -> identical pixel under the box.
      const a = pixelAt(decodePngRgba(plain), 20, 20);
      const b = pixelAt(decodePngRgba(withEmptyMask), 20, 20);
      expect(b).toEqual(a);
    });
  });
}
