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
import { launchForBackend } from "./_helpers.js";
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

// Minimal PNG decoder: returns RGBA pixels for an 8-bit truecolor-alpha
// or truecolor image. Sufficient for screenshots produced by the
// browser (which emit RGBA / RGB PNGs).
function decodePngRgba(buf: Buffer): { width: number; height: number; pixels: Uint8Array } {
  // Defer to the platform's built-in decoder via createImageBitmap if
  // present; otherwise parse the IHDR + IDAT chunks with zlib inflate.
  const zlib = require("node:zlib");
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error("not a PNG");
  let off = 8;
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  const idat: Buffer[] = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off);
    const type = buf.toString("ascii", off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data.readUInt8(8);
      colorType = data.readUInt8(9);
    } else if (type === "IDAT") {
      idat.push(Buffer.from(data));
    } else if (type === "IEND") {
      break;
    }
    off += 12 + len;
  }
  if (bitDepth !== 8) throw new Error(`unsupported bit depth ${bitDepth}`);
  const channels = colorType === 6 ? 4 : colorType === 2 ? 3 : 0;
  if (channels === 0) throw new Error(`unsupported color type ${colorType}`);
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const pixels = new Uint8Array(width * height * 4);
  let prev = new Uint8Array(stride);
  let rp = 0;
  for (let y = 0; y < height; y++) {
    const filter = raw[rp++];
    const line = new Uint8Array(stride);
    for (let x = 0; x < stride; x++) {
      const rawByte = raw[rp++];
      const a = x >= channels ? line[x - channels] : 0;
      const b = prev[x];
      const c = x >= channels ? prev[x - channels] : 0;
      let val: number;
      switch (filter) {
        case 0: val = rawByte; break;
        case 1: val = rawByte + a; break;
        case 2: val = rawByte + b; break;
        case 3: val = rawByte + ((a + b) >> 1); break;
        case 4: {
          const p = a + b - c;
          const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
          const pr = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
          val = rawByte + pr;
          break;
        }
        default: throw new Error(`unsupported filter ${filter}`);
      }
      line[x] = val & 0xff;
    }
    for (let x = 0; x < width; x++) {
      const si = x * channels;
      const di = (y * width + x) * 4;
      pixels[di] = line[si];
      pixels[di + 1] = line[si + 1];
      pixels[di + 2] = line[si + 2];
      pixels[di + 3] = channels === 4 ? line[si + 3] : 255;
    }
    prev = line;
  }
  return { width, height, pixels };
}

function pixelAt(img: { width: number; pixels: Uint8Array }, x: number, y: number): [number, number, number] {
  const i = (y * img.width + x) * 4;
  return [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2]];
}

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
