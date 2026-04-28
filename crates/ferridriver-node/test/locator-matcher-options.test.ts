// Cluster 5 — locator matcher option bags (§7.17).
//
// Exercises the new option fields against a live cdp-pipe browser:
//   toBeInViewport({ ratio })
//   toHaveCSS({ pseudo })
//   toHaveScreenshot({ threshold, maxDiffPixels, maxDiffPixelRatio, ignore })
//   toMatchAriaSnapshot (improved structural-by-line match)

import { test, expect as bunExpect } from 'bun:test';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { chromium, type Browser } from '../index.js';
import { expect } from '../../../packages/ferridriver-test/src/expect';

let browser: Browser;

async function withPage(html: string, fn: (page: any) => Promise<void>): Promise<void> {
  if (!browser) browser = await chromium().launch();
  const ctx = browser.newContext();
  try {
    const page = await ctx.newPage();
    await page.setContent(html);
    await fn(page);
  } finally {
    await ctx.close();
  }
}

test('toBeInViewport without ratio accepts any overlap', async () => {
  await withPage(
    `<style>body { margin: 0; height: 5000px; }</style>
     <div id="hit" style="height: 100px"></div>`,
    async (page) => {
      const loc = page.locator('#hit');
      await expect(loc).toBeInViewport();
    },
  );
});

test('toBeInViewport({ ratio: 1 }) requires the full element visible', async () => {
  await withPage(
    `<style>body { margin: 0; height: 200px; }</style>
     <div id="big" style="height: 2000px"></div>`,
    async (page) => {
      const loc = page.locator('#big');
      // Only a fraction fits — drop the timeout so the failing
      // assertion bails fast rather than polling for the default 5s.
      await bunExpect(async () => expect(loc, 500).toBeInViewport({ ratio: 1 })).toThrow();
      await expect(loc).toBeInViewport({ ratio: 0.05 });
    },
  );
}, 10_000);

test('toHaveCSS({ pseudo }) targets ::before', async () => {
  await withPage(
    `<style>
       #x::before { content: "marker"; color: rgb(10, 20, 30); }
     </style>
     <div id="x"></div>`,
    async (page) => {
      const loc = page.locator('#x');
      await expect(loc).toHaveCSS('color', 'rgb(10, 20, 30)', { pseudo: '::before' });
    },
  );
});

test('toHaveScreenshot({ ignore: true }) short-circuits comparison', async () => {
  const snapDir = mkdtempSync(join(tmpdir(), 'ferri-cluster5-snap-'));
  process.env.SNAPSHOT_DIR = snapDir;
  try {
    await withPage(
      `<div id="box" style="width:50px;height:50px;background:red"></div>`,
      async (page) => {
        const loc = page.locator('#box');
        // Write a deliberately-wrong baseline so a non-ignored compare
        // would fail.
        writeFileSync(join(snapDir, 'wrong.png'), Buffer.from('not a png'));
        await expect(loc).toHaveScreenshot('wrong', { ignore: true });
      },
    );
  } finally {
    delete process.env.SNAPSHOT_DIR;
    rmSync(snapDir, { recursive: true, force: true });
  }
});

test('toMatchAriaSnapshot accepts ordered subset', async () => {
  await withPage(
    `<main>
       <h1 aria-label="Title">Title</h1>
       <button>Click</button>
       <ul>
         <li>One</li>
         <li>Two</li>
       </ul>
     </main>`,
    async (page) => {
      const loc = page.locator('main');
      await expect(loc).toMatchAriaSnapshot(`
        h1 "Title"
        button "Click"
        li "One"
        li "Two"
      `);
    },
  );
});

// ── §7.17 capture-time options ─────────────────────────────────────

function pngDimensions(png: Buffer): { width: number; height: number } {
  return { width: png.readUInt32BE(16), height: png.readUInt32BE(20) };
}

/// Coarse pixel-sample helper: re-applies the standard PNG filter
/// algorithms to the inflated IDAT stream so we can compare sampled
/// pixels for mask / style tests. Chrome's headless screenshots are
/// 8-bit RGBA at colour type 6, so we only support that path.
async function pixelAt(png: Buffer, x: number, y: number): Promise<[number, number, number, number]> {
  const zlib = await import('zlib');
  let off = 8; // skip PNG signature
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  const idat: Buffer[] = [];
  while (off < png.length) {
    const len = png.readUInt32BE(off);
    const type = png.subarray(off + 4, off + 8).toString('ascii');
    if (type === 'IHDR') {
      width = png.readUInt32BE(off + 8);
      height = png.readUInt32BE(off + 12);
      bitDepth = png[off + 16];
      colorType = png[off + 17];
    } else if (type === 'IDAT') {
      idat.push(png.subarray(off + 8, off + 8 + len));
    }
    off += 12 + len;
  }
  if (bitDepth !== 8) throw new Error(`pixelAt: unsupported bit depth ${bitDepth}`);
  // Bytes-per-pixel by colour type.
  const bpp = colorType === 6 ? 4 : colorType === 2 ? 3 : colorType === 4 ? 2 : 1;
  const inflated = zlib.inflateSync(Buffer.concat(idat));
  const scanlineLen = 1 + width * bpp;
  // Reverse-apply the per-row filter so we can read the recovered RGBA.
  const recon = Buffer.alloc(width * height * bpp);
  for (let row = 0; row < height; row++) {
    const filter = inflated[row * scanlineLen];
    const rowOff = row * scanlineLen + 1;
    const reconOff = row * width * bpp;
    for (let i = 0; i < width * bpp; i++) {
      const raw = inflated[rowOff + i];
      const left = i >= bpp ? recon[reconOff + i - bpp] : 0;
      const up = row > 0 ? recon[reconOff - width * bpp + i] : 0;
      const upLeft = row > 0 && i >= bpp ? recon[reconOff - width * bpp + i - bpp] : 0;
      let value: number;
      switch (filter) {
        case 0: // None
          value = raw;
          break;
        case 1: // Sub
          value = (raw + left) & 0xff;
          break;
        case 2: // Up
          value = (raw + up) & 0xff;
          break;
        case 3: // Average
          value = (raw + ((left + up) >> 1)) & 0xff;
          break;
        case 4: {
          // Paeth
          const p = left + up - upLeft;
          const pa = Math.abs(p - left);
          const pb = Math.abs(p - up);
          const pc = Math.abs(p - upLeft);
          const pred = pa <= pb && pa <= pc ? left : pb <= pc ? up : upLeft;
          value = (raw + pred) & 0xff;
          break;
        }
        default:
          throw new Error(`pixelAt: unknown filter ${filter} on row ${row}`);
      }
      recon[reconOff + i] = value;
    }
  }
  const pixOff = (y * width + x) * bpp;
  if (bpp === 4) return [recon[pixOff], recon[pixOff + 1], recon[pixOff + 2], recon[pixOff + 3]];
  return [recon[pixOff], recon[pixOff + 1], recon[pixOff + 2], 255];
}

// Pulling in `__snapshots__/<name>.png` requires the comparator to
// see SNAPSHOT_DIR in libc's environ. Bun mutates process.env in JS
// memory but doesn't sync to libc, so we run each capture in a
// pre-chdir'd temp dir and read the default `__snapshots__/<name>.png`
// the matcher writes when no baseline is present.
async function withSnapshotDir<T>(prefix: string, fn: (snapDir: string) => Promise<T>): Promise<T> {
  const snapDir = mkdtempSync(join(tmpdir(), prefix));
  const cwd = process.cwd();
  process.chdir(snapDir);
  try {
    return await fn(snapDir);
  } finally {
    process.chdir(cwd);
    rmSync(snapDir, { recursive: true, force: true });
  }
}

test('toHaveScreenshot { clip } crops the captured PNG to the requested rect', async () => {
  await withSnapshotDir('ferri-cluster5-clip-', async (snapDir) => {
    await withPage(
      `<style>body{margin:0;padding:0;background:white}</style>
       <div id="big" style="width:300px;height:300px;background:red"></div>`,
      async (page) => {
        const loc = page.locator('#big');
        await expect(loc).toHaveScreenshot('clipped', {
          clip: { x: 0, y: 0, width: 80, height: 80 },
        });
        const written = readFileSync(join(snapDir, '__snapshots__', 'clipped.png'));
        const dims = pngDimensions(written);
        bunExpect(dims.width).toBe(80);
        bunExpect(dims.height).toBe(80);
      },
    );
  });
});

test('toHaveScreenshot { mask } overlays magenta on masked elements before capture', async () => {
  await withSnapshotDir('ferri-cluster5-mask-', async (snapDir) => {
    await withPage(
      `<style>body{margin:0;padding:0}</style>
       <div id="root" style="width:60px;height:60px;background:white">
         <div id="hidden" style="width:30px;height:30px;background:black"></div>
       </div>`,
      async (page) => {
        const loc = page.locator('#root');
        // Mask the inner black div with magenta (#FF00FF). Without the
        // mask, the captured PNG's top-left 30x30 would be black; with
        // the mask applied, the same region renders magenta.
        await expect(loc).toHaveScreenshot('masked', {
          mask: ['#hidden'],
          maskColor: '#FF00FF',
          // Disable animations so the capture is deterministic regardless
          // of any incidental transitions.
          animations: 'disabled',
        });
        const written = readFileSync(join(snapDir, '__snapshots__', 'masked.png'));
        // Sample a pixel inside the masked region (inset to dodge
        // sub-pixel anti-aliasing at the edge).
        const px = await pixelAt(written, 5, 5);
        bunExpect(px[0]).toBeGreaterThan(200); // red high
        bunExpect(px[1]).toBeLessThan(50); // green low
        bunExpect(px[2]).toBeGreaterThan(200); // blue high
      },
    );
  });
});

test('toHaveScreenshot { stylePath } injects user styles before capture and removes them after', async () => {
  await withSnapshotDir('ferri-cluster5-style-', async (snapDir) => {
    const styleFile = join(snapDir, 'override.css');
    writeFileSync(styleFile, '#root { background: blue !important; }');
    await withPage(
      `<style>body{margin:0;padding:0}</style>
       <div id="root" style="width:40px;height:40px;background:white"></div>`,
      async (page) => {
        const loc = page.locator('#root');
        await expect(loc).toHaveScreenshot('styled', { stylePath: styleFile });
        const written = readFileSync(join(snapDir, '__snapshots__', 'styled.png'));
        // Center pixel should be blue thanks to the injected style.
        const px = await pixelAt(written, 20, 20);
        bunExpect(px[2]).toBeGreaterThan(200); // blue channel high
        bunExpect(px[0]).toBeLessThan(50); // red low
        // The capture wrapper must remove the injected <style> after
        // the shot so user code doesn't see leftover state.
        const remaining = await page.evaluate('document.querySelectorAll("style[data-ferridriver-screenshot-capture]").length');
        bunExpect(Number(remaining)).toBe(0);
      },
    );
  });
});

test('toMatchAriaSnapshot rejects out-of-order expectations', async () => {
  await withPage(
    `<main>
       <h1>First</h1>
       <button>Second</button>
     </main>`,
    async (page) => {
      const loc = page.locator('main');
      // Rejecting reversed order is what the new structural-cursor
      // walker buys us over the old line-substring loop. Drop the
      // timeout so the assertion bails fast rather than polling for
      // the default 5s.
      await bunExpect(async () =>
        expect(loc, 500).toMatchAriaSnapshot(`
          button "Second"
          h1 "First"
        `),
      ).toThrow();
    },
  );
});
