/**
 * Tasks 3.15 + 3.16 — locator.getAttribute raw return + locator.waitFor
 * attached-vs-visible state split.
 *
 * Playwright signatures (cloned at
 * /tmp/playwright/packages/playwright-core/src/client/locator.ts):
 *
 *   locator.getAttribute(name): Promise<null | string>
 *   locator.waitFor({ state?: 'attached' | 'detached' | 'visible' | 'hidden', timeout? }): Promise<void>
 *
 * `attached` resolves on DOM presence alone, `visible` additionally
 * requires computed style to render. Previously the two were conflated.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] getAttribute + waitFor states`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    // ── 3.15 getAttribute returns raw string, not JSON-stringified ───────

    it("getAttribute returns the raw attribute string", async () => {
      await page.setContent(
        '<input id="x" type="number" value="42" data-flag="true" aria-label="label-text">',
      );
      // Every attribute is stored as string by the DOM, even when the
      // value looks numeric or boolean. getAttribute must return that
      // raw string verbatim — no JSON quoting, no type coercion.
      expect(await page.locator("#x").getAttribute("value")).toBe("42");
      expect(await page.locator("#x").getAttribute("data-flag")).toBe("true");
      expect(await page.locator("#x").getAttribute("type")).toBe("number");
      expect(await page.locator("#x").getAttribute("aria-label")).toBe("label-text");
    });

    it("getAttribute returns null for missing attributes", async () => {
      await page.setContent('<div id="plain">x</div>');
      expect(await page.locator("#plain").getAttribute("data-missing")).toBeNull();
    });

    it("getAttribute preserves empty-string attribute value", async () => {
      // <input disabled> → getAttribute('disabled') returns "" per DOM spec.
      await page.setContent('<input id="i" disabled>');
      expect(await page.locator("#i").getAttribute("disabled")).toBe("");
    });

    // ── 3.16 waitFor: attached is distinct from visible ──────────────────

    it("waitFor({ state: 'attached' }) resolves on DOM presence even when display:none", async () => {
      // Render a hidden element immediately.
      await page.setContent(
        '<div id="hidden-dom" style="display:none">present but invisible</div>',
      );
      // `attached` must resolve — the element IS in the DOM.
      await page.locator("#hidden-dom").waitFor({ state: "attached", timeout: 2000 });
      // Sanity: `visible` should NOT resolve within the same budget
      // because the element has display:none.
      await expect(
        page.locator("#hidden-dom").waitFor({ state: "visible", timeout: 300 }),
      ).rejects.toThrow(/timeout/i);
    });

    it("waitFor({ state: 'visible' }) waits for computed-style visibility", async () => {
      // Element initially hidden; flip to visible after a delay.
      await page.setContent(
        '<div id="late" style="display:none">later</div>' +
          "<script>setTimeout(() => document.getElementById('late').style.display = 'block', 200);</script>",
      );
      await page.locator("#late").waitFor({ state: "visible", timeout: 2000 });
      expect(await page.locator("#late").isVisible()).toBe(true);
    });

    it("waitFor({ state: 'detached' }) only resolves once element leaves DOM", async () => {
      await page.setContent(
        '<div id="gone">bye</div>' +
          "<script>setTimeout(() => document.getElementById('gone').remove(), 150);</script>",
      );
      await page.locator("#gone").waitFor({ state: "detached", timeout: 2000 });
    });

    it("waitFor({ state: 'hidden' }) resolves for DOM-present-but-invisible elements", async () => {
      // Playwright parity: `hidden` is satisfied by `display:none` on a
      // DOM-present element (not just detachment).
      await page.setContent('<div id="h" style="display:none">x</div>');
      await page.locator("#h").waitFor({ state: "hidden", timeout: 2000 });
    });

    it("waitFor({ state: 'hidden' }) also resolves when element is detached", async () => {
      await page.setContent(
        '<div id="vanish">present</div>' +
          "<script>setTimeout(() => document.getElementById('vanish').remove(), 150);</script>",
      );
      await page.locator("#vanish").waitFor({ state: "hidden", timeout: 2000 });
    });

    it("waitFor with unknown state rejects with invalid-argument", async () => {
      await page.setContent("<div id=\"any\">x</div>");
      await expect(
        page.locator("#any").waitFor({ state: "bogus" as any, timeout: 500 }),
      ).rejects.toThrow();
    });
  });
}
