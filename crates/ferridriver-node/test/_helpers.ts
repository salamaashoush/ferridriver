// Test helpers: backend-matrix launcher.
//
// `Browser.launch({ backend })` no longer exists — the new
// Playwright-shaped API is `chromium()` / `firefox()` / `webkit()`.
// `cdp-raw` access is via `chromium({ transport: 'ws' })`. This
// helper preserves the per-backend test matrix that every NAPI test
// file relies on.

import { inflateSync } from "node:zlib";
import { chromium, firefox, webkit, type Browser } from "../index.js";

/**
 * Launch a browser for a backend identifier. Used by the per-backend
 * matrix in NAPI tests (`for (const backend of BACKENDS) ...`).
 *
 * - `cdp-pipe` -> `chromium().launch()`
 * - `cdp-raw`  -> `chromium({ transport: 'ws' }).launch()`
 * - `bidi`     -> `firefox().launch()`
 * - `webkit`   -> `webkit().launch()` (macOS only)
 */
export function launchForBackend(backend: string): Promise<Browser> {
  switch (backend) {
    case "cdp-pipe":
      return chromium().launch();
    case "cdp-raw":
      return chromium({ transport: "ws" }).launch();
    case "bidi":
      return firefox().launch();
    case "webkit":
      return webkit().launch();
    default:
      throw new Error(`Unknown backend: ${backend}`);
  }
}

// Minimal PNG decoder: returns RGBA pixels for an 8-bit truecolor-alpha
// or truecolor image. Sufficient for screenshots produced by the
// browser (which emit RGBA / RGB PNGs).
export function decodePngRgba(buf: Buffer): { width: number; height: number; pixels: Uint8Array } {
  // Defer to the platform's built-in decoder via createImageBitmap if
  // present; otherwise parse the IHDR + IDAT chunks with zlib inflate.
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
  const raw = inflateSync(Buffer.concat(idat));
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

export function pixelAt(img: { width: number; pixels: Uint8Array }, x: number, y: number): [number, number, number] {
  const i = (y * img.width + x) * 4;
  return [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2]];
}
