/**
 * NAPI parity tests for `Locator.drop(payload, options?)`.
 *
 * Mirrors Playwright's `locator.drop` (client/locator.ts:129 ->
 * frame._drop -> server dom.ts::_drop): build a DataTransfer carrying the
 * payload's File objects + data entries, dispatch dragenter/dragover/drop
 * on the target at its actionability point. Each test observes a
 * DOM-side effect that ONLY occurs when the payload actually reached the
 * page's drop handler (Rule 9), not just that the call resolved.
 *
 * Gated to CDP backends here; the logic is identical across backends
 * since the whole sequence runs page-side via call_js_fn_value.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

// A drop zone that ACCEPTS (its dragover calls preventDefault) and records
// the dropped data + files back into document.title for assertion.
const ACCEPT_HTML = `
<div id="zone" style="width:200px;height:200px;background:#eee">drop here</div>
<script>
  const z = document.getElementById('zone');
  z.addEventListener('dragover', (e) => { e.preventDefault(); });
  z.addEventListener('drop', async (e) => {
    e.preventDefault();
    const dt = e.dataTransfer;
    const text = dt.getData('text/plain');
    const names = [];
    for (const f of dt.files) names.push(f.name + ':' + (await f.text()));
    document.title = 'data=' + text + ';files=' + names.join('|');
  });
</script>`;

// A drop zone that REJECTS: its dragover never calls preventDefault, so
// the drop is treated as not accepted.
const REJECT_HTML = `
<div id="zone" style="width:200px;height:200px;background:#eee">no drop</div>`;

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
  describe(`[${backend}] Locator.drop(payload, options?)`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("drops a data payload, page reads it via DataTransfer.getData", async () => {
      await page.goto(await dataUrl(ACCEPT_HTML), null);
      await page.locator("#zone").drop({ data: { "text/plain": "hello-drop" } });
      const title = await pollTitle(page, (t) => t.startsWith("data="));
      expect(title).toContain("data=hello-drop");
      expect(title).toContain("files=");
    });

    it("drops a FilePayload, page reads name + bytes from DataTransfer.files", async () => {
      await page.goto(await dataUrl(ACCEPT_HTML), null);
      await page.locator("#zone").drop({
        files: { name: "card.txt", mimeType: "text/plain", buffer: Buffer.from("payload-bytes") },
      });
      const title = await pollTitle(page, (t) => t.includes("card.txt"));
      expect(title).toContain("card.txt:payload-bytes");
    });

    it("rejects when the target's dragover does not preventDefault", async () => {
      await page.goto(await dataUrl(REJECT_HTML), null);
      await expect(
        page.locator("#zone").drop({ data: { "text/plain": "x" } }),
      ).rejects.toThrow(/did not accept the drop/);
    });
  });
}
