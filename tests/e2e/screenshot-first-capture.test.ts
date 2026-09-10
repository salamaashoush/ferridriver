import { inflateSync } from 'node:zlib';
import { test, expect } from '@ferridriver/test';

function firstPixel(bytes: Uint8Array): number[] {
  const png = Uint8Array.from(bytes);
  const view = new DataView(png.buffer, png.byteOffset, png.byteLength);
  expect(view.getUint32(0)).toBe(0x89504e47);
  expect(view.getUint32(16)).toBe(1280);
  expect(view.getUint32(20)).toBe(720);
  expect(png[24]).toBe(8);
  expect([2, 6].includes(png[25])).toBe(true);
  const chunks: Uint8Array[] = [];
  let length = 0;
  for (let offset = 8; offset < png.length;) {
    const size = view.getUint32(offset);
    if (view.getUint32(offset + 4) === 0x49444154) {
      const chunk = png.slice(offset + 8, offset + 8 + size);
      chunks.push(chunk);
      length += chunk.length;
    }
    offset += size + 12;
  }
  const compressed = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    compressed.set(chunk, offset);
    offset += chunk.length;
  }
  const pixels = inflateSync(compressed);
  return Array.from(pixels.slice(1, 4));
}

for (const subject of ['page', 'locator']) {
  test(`first ${subject} screenshots contain rendered pixels during concurrent creation`, async ({ browser }) => {
    await Promise.all(Array.from({ length: 16 }, async (_, index) => {
      const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
      try {
        const page = await context.newPage();
        const html = '<style>body{margin:0}#target{width:1280px;height:720px;background:#00ff00}</style><div id=target></div>';
        await page.goto(`data:text/html,${encodeURIComponent(html)}`);
        await test.step(`first capture ${index}`, async () => {
          const image = subject === 'page'
            ? await page.screenshot({ scale: 'css' })
            : await page.locator('#target').screenshot({ scale: 'css' });
          expect(firstPixel(image)).toEqual([0, 255, 0]);
        });
      } finally {
        await context.close();
      }
    }));
  });
}
