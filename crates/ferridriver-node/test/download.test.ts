/**
 * NAPI parity tests for the Download lifecycle handle.
 *
 * `page.waitForEvent('download')` returns a live `Download` class
 * instance — `url()`, `suggestedFilename()`, `page()`, async `path()` /
 * `saveAs()` / `cancel()` / `delete()` / `failure()`. The dispatch
 * bypasses the broadcast emitter and routes through the Rust-core
 * `DownloadManager::add_handler` path so the claim is synchronous with
 * the backend's `Browser.downloadWillBegin` event.
 *
 * Gated to CDP backends here; the Rust integration suite
 * (`tests/backends_support/download.rs`) covers CDP + BiDi + the
 * documented WebKit gap (stock `WKWebView` routes downloads through
 * `WKDownloadDelegate` in the host subprocess and those events don't
 * yet flow through our IPC).
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { readFileSync, mkdtempSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createServer, type Server } from "node:http";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

type Download = {
  url(): string;
  suggestedFilename(): string;
  page(): unknown;
  path(): Promise<string>;
  saveAs(path: string): Promise<void>;
  cancel(): Promise<void>;
  delete(): Promise<void>;
  failure(): Promise<string | null>;
};

/// Local HTTP server that serves a landing page containing an anchor
/// to `/file.bin`, which itself is a `Content-Disposition: attachment`
/// response with the supplied payload bytes — the canonical download
/// trigger on Chrome and Firefox.
async function startDownloadServer(payload: Buffer): Promise<{ base: string; close: () => Promise<void> }> {
  const server: Server = createServer((req, res) => {
    if (req.url && req.url.startsWith("/file.bin")) {
      res.writeHead(200, {
        "content-type": "application/octet-stream",
        "content-disposition": 'attachment; filename="greeting.txt"',
        "content-length": String(payload.length),
        "connection": "close",
      });
      res.end(payload);
      return;
    }
    const html =
      '<!doctype html><html><body>' +
      '<a id="dl" href="/file.bin">download</a>' +
      '</body></html>';
    res.writeHead(200, {
      "content-type": "text/html",
      "content-length": String(html.length),
      "connection": "close",
    });
    res.end(html);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  const port = typeof address === "object" && address ? address.port : 0;
  const base = `http://127.0.0.1:${port}`;
  return {
    base,
    close: () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve());
      }),
  };
}

for (const backend of BACKENDS) {
  describe(`[${backend}] Download as first-class handle (§2.10)`, () => {
    let browser: Browser;
    let page: Page;
    const tmpDir = mkdtempSync(join(tmpdir(), "ferridriver-dl-napi-"));

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("waitForEvent('download') + saveAs copies bytes byte-for-byte", async () => {
      const payload = Buffer.from("hello download world");
      const { base, close } = await startDownloadServer(payload);
      try {
        await page.goto(base, null);
        const savePath = join(tmpDir, `save-${backend}.bin`);
        const waiter = page.waitForEvent("download", 15_000);
        // Fire the click; don't await it before the waiter because the
        // event can arrive before the click call returns on some
        // protocols.
        const clickPromise = page.click("#dl");
        const download = (await waiter) as unknown as Download;
        expect(download.url()).toContain("/file.bin");
        expect(download.suggestedFilename()).toBe("greeting.txt");
        await download.saveAs(savePath);
        await clickPromise.catch(() => {
          // Click may reject with a navigation-aborted error when the
          // attachment response starts the download — we don't care.
        });
        const saved = readFileSync(savePath);
        expect(saved.equals(payload)).toBe(true);
      } finally {
        await close();
      }
    });

    it("path() resolves to the browser-written file", async () => {
      const payload = Buffer.from("payload-for-path");
      const { base, close } = await startDownloadServer(payload);
      try {
        await page.goto(base, null);
        const waiter = page.waitForEvent("download", 15_000);
        const clickPromise = page.click("#dl");
        const download = (await waiter) as unknown as Download;
        const p = await download.path();
        await clickPromise.catch(() => {});
        const bytes = readFileSync(p);
        expect(bytes.equals(payload)).toBe(true);
      } finally {
        await close();
      }
    });

    it("cancel() surfaces failure() === 'canceled'", async () => {
      const payload = Buffer.from("cancel-me-please");
      const { base, close } = await startDownloadServer(payload);
      try {
        await page.goto(base, null);
        const waiter = page.waitForEvent("download", 15_000);
        const clickPromise = page.click("#dl");
        const download = (await waiter) as unknown as Download;
        await download.cancel();
        await clickPromise.catch(() => {});
        const failure = await download.failure();
        expect(failure).toBe("canceled");
      } finally {
        await close();
      }
    });

    it("waitForEvent timeout when no download begins", async () => {
      await page.goto("data:text/html,<h1>quiet</h1>", null);
      await expect(page.waitForEvent("download", 500)).rejects.toThrow(/[Tt]imeout|waiting for download/);
    });
  });
}
