/**
 * NAPI parity tests for the FileChooser lifecycle handle.
 *
 * `page.waitForEvent('filechooser')` returns a live `FileChooser`
 * class instance — `element()`, `isMultiple()`, and async
 * `setFiles(files, options?)`. The dispatch bypasses the broadcast
 * emitter and routes through the Rust-core
 * `FileChooserManager::add_handler` path so the claim is synchronous
 * with the browser's `Page.fileChooserOpened` event — no grace
 * window, no race.
 *
 * Gated to CDP backends here; the Rust integration suite
 * (`tests/backends_support/file_chooser.rs`) covers CDP + BiDi + the
 * documented WebKit gap (stock `WKWebView` exposes no public API for
 * intercepting `<input type=file>` clicks).
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { writeFileSync, mkdtempSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

const SINGLE_FORM_HTML = `
<form id="f">
  <input id="i" type="file" name="f" />
  <button id="b" type="button">pick</button>
</form>
<script>
  const i = document.getElementById('i');
  const b = document.getElementById('b');
  b.addEventListener('click', () => i.click());
  i.addEventListener('change', () => {
    const files = i.files;
    const count = files.length;
    const first = count > 0 ? files[0].name : '';
    document.title = 'count=' + count + ';first=' + first;
  });
</script>`;

const MULTIPLE_FORM_HTML = `
<form id="f">
  <input id="i" type="file" name="f" multiple />
  <button id="b" type="button">pick</button>
</form>
<script>
  const i = document.getElementById('i');
  const b = document.getElementById('b');
  b.addEventListener('click', () => i.click());
  i.addEventListener('change', () => {
    const files = i.files;
    const names = [];
    for (let k = 0; k < files.length; k++) names.push(files[k].name);
    document.title = 'count=' + files.length + ';names=' + names.join('|');
  });
</script>`;

/// Variant that reports the uploaded file's name + size + decoded
/// text so a test can assert a `FilePayload`'s bytes reached the page.
const PAYLOAD_FORM_HTML = `
<form id="f">
  <input id="i" type="file" name="f" />
  <button id="b" type="button">pick</button>
</form>
<script>
  const i = document.getElementById('i');
  const b = document.getElementById('b');
  b.addEventListener('click', () => i.click());
  i.addEventListener('change', async () => {
    const f = i.files[0];
    const text = await f.text();
    document.title = 'name=' + f.name + ';size=' + f.size + ';text=' + text;
  });
</script>`;

type FileChooser = {
  element(): unknown;
  isMultiple(): boolean;
  setFiles(
    files: string | string[] | { name: string; mimeType: string; buffer: Buffer | Uint8Array },
    options?: unknown,
  ): Promise<void>;
};

async function dataUrl(html: string): Promise<string> {
  return "data:text/html," + encodeURIComponent(html);
}

async function pollTitle(page: Page, predicate: (t: string) => boolean, deadlineMs = 3000): Promise<string> {
  const deadline = Date.now() + deadlineMs;
  while (Date.now() < deadline) {
    const t = await page.title();
    if (predicate(t)) return t;
    await new Promise((r) => setTimeout(r, 20));
  }
  return page.title();
}

for (const backend of BACKENDS) {
  describe(`[${backend}] FileChooser as first-class handle (§2.11)`, () => {
    let browser: Browser;
    let page: Page;
    const tmpDir = mkdtempSync(join(tmpdir(), "ferridriver-fc-napi-"));

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("waitForEvent('filechooser') + isMultiple + setFiles(string)", async () => {
      await page.goto(await dataUrl(SINGLE_FORM_HTML), null);
      const path = join(tmpDir, "a.txt");
      writeFileSync(path, "alpha");
      const waiter = page.waitForEvent("filechooser", 10_000);
      // Don't await the click before the waiter — the event may
      // already have fired by the time click returns.
      const clickPromise = page.click("#b");
      const chooser = (await waiter) as unknown as FileChooser;
      expect(chooser.isMultiple()).toBe(false);
      await chooser.setFiles(path);
      await clickPromise;
      const title = await pollTitle(page, (t) => t.startsWith("count="));
      expect(title).toBe("count=1;first=a.txt");
    });

    it("setFiles(string[]) uploads multiple on <input multiple>", async () => {
      await page.goto(await dataUrl(MULTIPLE_FORM_HTML), null);
      const p1 = join(tmpDir, "alpha-multi.txt");
      const p2 = join(tmpDir, "beta-multi.txt");
      writeFileSync(p1, "AA");
      writeFileSync(p2, "BB");
      const waiter = page.waitForEvent("filechooser", 10_000);
      const clickPromise = page.click("#b");
      const chooser = (await waiter) as unknown as FileChooser;
      expect(chooser.isMultiple()).toBe(true);
      await chooser.setFiles([p1, p2]);
      await clickPromise;
      const title = await pollTitle(page, (t) => t.startsWith("count="));
      expect(title === "count=2;names=alpha-multi.txt|beta-multi.txt" ||
             title === "count=2;names=beta-multi.txt|alpha-multi.txt").toBe(true);
    });

    it("setFiles(FilePayload) uploads an in-memory buffer", async () => {
      // Use the payload-form variant so the page's change handler
      // reports the payload bytes back via `document.title`.
      await page.goto(await dataUrl(PAYLOAD_FORM_HTML), null);
      const waiter = page.waitForEvent("filechooser", 10_000);
      const clickPromise = page.click("#b");
      const chooser = (await waiter) as unknown as FileChooser;
      await chooser.setFiles({
        name: "greeting.txt",
        mimeType: "text/plain",
        buffer: Buffer.from("hello"),
      });
      await clickPromise;
      const title = await pollTitle(page, (t) => t.startsWith("name="));
      expect(title).toContain("name=greeting.txt");
      expect(title).toContain("size=5");
      expect(title).toContain("text=hello");
    });

    it("waitForEvent timeout when no chooser opens", async () => {
      // No click, no filechooser ever opens. 500ms is enough to
      // prove the timeout path fires without dragging the suite.
      await page.goto(await dataUrl("<h1>quiet</h1>"), null);
      await expect(page.waitForEvent("filechooser", 500)).rejects.toThrow(/[Tt]imeout|waiting for filechooser/);
    });
  });
}
