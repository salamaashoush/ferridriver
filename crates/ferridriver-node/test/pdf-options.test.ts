/**
 * Full Playwright PDFOptions surface — task #3.4.
 *
 * Playwright signature (cloned at
 * /tmp/playwright/packages/playwright-core/src/client/page.ts):
 *
 *   pdf(options?: PDFOptions): Promise<Buffer>
 *
 *   PDFOptions = {
 *     scale, displayHeaderFooter, headerTemplate, footerTemplate,
 *     printBackground, landscape, pageRanges, format, width, height,
 *     margin: { top, right, bottom, left },
 *     path, preferCSSPageSize, tagged, outline
 *   }
 *
 * All 15 fields must reach CDP Page.printToPDF and affect the output.
 * We don't parse the PDF bytes here — instead we assert:
 *   - every field flows through without error
 *   - different options produce different byte counts (or at least a
 *     different prefix) for options that visibly affect rendering
 *   - `path` writes the file to disk with identical bytes to the return
 *     value
 *   - `format` with an unknown value surfaces as an error
 *
 * PDF is a CDP-only capability; WebKit has no analogue so we skip that
 * backend (cdp-pipe + cdp-raw only).
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type Server } from "node:http";
import { mkdtempSync, rmSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const FIXTURE = `<!DOCTYPE html>
<html>
<head>
  <title>pdf fixture</title>
  <style>body { font-family: sans-serif; background: #fafafa; }</style>
</head>
<body>
  <h1>Page 1 heading</h1>
  <p>This is the first page of content. It should render in the PDF.</p>
  <div style="page-break-before: always"></div>
  <h1>Page 2 heading</h1>
  <p>Second page content.</p>
  <div style="page-break-before: always"></div>
  <h1>Page 3 heading</h1>
  <p>Third page content.</p>
</body>
</html>`;

let testServer: Server;
let testUrl: string;
let tmpDir: string;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(FIXTURE);
  });
  await new Promise<void>((resolve) => {
    testServer.listen(0, "127.0.0.1", () => {
      const addr = testServer.address() as { port: number };
      testUrl = `http://127.0.0.1:${addr.port}`;
      resolve();
    });
  });
  tmpDir = mkdtempSync(join(tmpdir(), "ferri-pdf-"));
});

afterAll(() => {
  testServer?.close();
  if (tmpDir && existsSync(tmpDir)) rmSync(tmpDir, { recursive: true, force: true });
});

const isPdf = (buf: Buffer) => buf.length > 4 && buf.subarray(0, 4).toString("ascii") === "%PDF";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw"];

for (const backend of BACKENDS) {
  describe(`[${backend}] PDFOptions parity`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("page.pdf() with no options returns a valid PDF", async () => {
      const buf = await page.pdf();
      expect(buf).toBeInstanceOf(Buffer);
      expect(isPdf(buf)).toBe(true);
      expect(buf.length).toBeGreaterThan(500);
    });

    it("format: 'A4' produces a different size than format: 'Letter'", async () => {
      const a4 = await page.pdf({ format: "A4" });
      const letter = await page.pdf({ format: "Letter" });
      // A4 (8.27 x 11.7) and Letter (8.5 x 11) differ in physical size;
      // the encoded PDFs MUST contain different page dimensions, which
      // produces different byte content even for identical HTML.
      expect(isPdf(a4)).toBe(true);
      expect(isPdf(letter)).toBe(true);
      // Both contain "MediaBox" but with different numbers.
      expect(a4.equals(letter)).toBe(false);
    });

    it("unknown format keyword rejects with a clear error", async () => {
      await expect(page.pdf({ format: "A99" })).rejects.toThrow(/paper format/i);
    });

    it("width + height as strings with units ('8in', '10in') override page size", async () => {
      const buf = await page.pdf({ width: "8in", height: "10in" });
      expect(isPdf(buf)).toBe(true);
    });

    it("width + height as numbers are interpreted as CSS pixels", async () => {
      // 816px × 1056px == US Letter at 96 DPI (8.5 x 11 in).
      const buf = await page.pdf({ width: 816, height: 1056 });
      expect(isPdf(buf)).toBe(true);
    });

    it("margin with mixed unit strings and numbers round-trips", async () => {
      const buf = await page.pdf({
        format: "Letter",
        margin: { top: "1in", right: 72, bottom: "2.54cm", left: "25.4mm" },
      });
      expect(isPdf(buf)).toBe(true);
    });

    it("landscape: true differs from landscape: false", async () => {
      const portrait = await page.pdf({ format: "Letter", landscape: false });
      const landscape = await page.pdf({ format: "Letter", landscape: true });
      expect(isPdf(landscape)).toBe(true);
      expect(portrait.equals(landscape)).toBe(false);
    });

    it("printBackground: true produces different output than false", async () => {
      const noBg = await page.pdf({ format: "Letter", printBackground: false });
      const withBg = await page.pdf({ format: "Letter", printBackground: true });
      expect(isPdf(withBg)).toBe(true);
      expect(noBg.equals(withBg)).toBe(false);
    });

    it("scale affects output", async () => {
      const s1 = await page.pdf({ format: "Letter", scale: 1.0 });
      const s15 = await page.pdf({ format: "Letter", scale: 1.5 });
      expect(isPdf(s15)).toBe(true);
      expect(s1.equals(s15)).toBe(false);
    });

    it("pageRanges limits output to specified pages", async () => {
      const all = await page.pdf({ format: "Letter" });
      const firstOnly = await page.pdf({ format: "Letter", pageRanges: "1" });
      expect(isPdf(firstOnly)).toBe(true);
      // The ranged PDF should be smaller than the full 3-page one.
      expect(firstOnly.length).toBeLessThan(all.length);
    });

    it("displayHeaderFooter + header/footer templates render", async () => {
      const buf = await page.pdf({
        format: "Letter",
        displayHeaderFooter: true,
        headerTemplate: '<div style="font-size:10px">HEADER</div>',
        footerTemplate: '<div style="font-size:10px">FOOTER</div>',
        margin: { top: "1in", bottom: "1in" },
      });
      expect(isPdf(buf)).toBe(true);
    });

    it("path option writes PDF to disk with identical bytes", async () => {
      const outPath = join(tmpDir, "nested", "subdir", "out.pdf");
      const buf = await page.pdf({ format: "Letter", path: outPath });
      expect(isPdf(buf)).toBe(true);
      expect(existsSync(outPath)).toBe(true);
      const onDisk = readFileSync(outPath);
      expect(onDisk.equals(buf)).toBe(true);
    });

    it("preferCSSPageSize, outline, tagged fields flow through without error", async () => {
      // We can't easily assert the visible effect of each here — but if the
      // NAPI binding isn't wired, the CDP call errors out. Assert the PDF
      // still generates cleanly for every one of these "structural" flags.
      const buf = await page.pdf({
        preferCSSPageSize: true,
        outline: true,
        tagged: true,
      });
      expect(isPdf(buf)).toBe(true);
    });
  });
}
