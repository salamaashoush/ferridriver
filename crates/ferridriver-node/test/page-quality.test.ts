// NAPI coverage for page.checkPageQuality(), the seven Lighthouse
// audits that score a page as it stands.
//
// Not a Playwright method. What these agree with is checked elsewhere
// (`just quality-diff`, and the recorded gate in ferridriver-perf); this
// is the binding surface: the option bag reaches Rust, the report comes
// back with its category, explanation and items intact.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND ? [process.env.FERRIDRIVER_BACKEND] : ["cdp-pipe"];

const SOUND =
  "<!doctype html><html lang='en'><head><title>t</title>" +
  "<meta name='description' content='A page with a description'></head>" +
  "<body><main><p><a href='https://example.com/pricing'>See our pricing</a></p></main></body></html>";

for (const backend of BACKENDS) {
  describe(`page quality audits [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("reports all seven with a category on each", async () => {
      await page.setContent(SOUND);
      const report = await page.checkPageQuality();

      expect(report.audits.map((a) => a.id)).toEqual([
        "doctype",
        "meta-description",
        "crawlable-anchors",
        "link-text",
        "image-aspect-ratio",
        "image-size-responsive",
        "paste-preventing-inputs",
      ]);
      expect(report.audits.every((a) => a.passed)).toBe(true);
      expect(report.audits.find((a) => a.id === "meta-description")!.category).toBe("seo");
      expect(report.audits.find((a) => a.id === "doctype")!.category).toBe("best-practices");
    });

    it("a failure carries its explanation and the elements behind it", async () => {
      // setContent leaves the document at about:blank, so the href here
      // is absolute; a relative one would resolve to nothing and fail
      // crawlable-anchors, which is a different audit's business.
      await page.setContent(
        "<html lang='en'><head><title>t</title></head><body><main>" +
          "<p><a href='https://example.com/pricing'>read more</a></p>" +
          "<input id='card' type='text' onpaste='return false'>" +
          "</main></body></html>",
      );
      const report = await page.checkPageQuality();
      const audit = (id: string) => report.audits.find((a) => a.id === id)!;

      expect(audit("doctype").passed).toBe(false);
      expect(audit("doctype").explanation).toBe("Document must contain a doctype");
      // No description element at all, which is the branch upstream
      // gives no explanation for.
      expect(audit("meta-description").passed).toBe(false);
      expect(audit("meta-description").explanation == null).toBe(true);

      expect(audit("link-text").items.map((i) => i.detail)).toEqual(["read more"]);
      expect(audit("paste-preventing-inputs").items[0].selector).toBe("#card");
      expect(audit("paste-preventing-inputs").items[0].snippet).toContain("<input");
    });

    it("only narrows the run", async () => {
      await page.setContent(SOUND);
      const report = await page.checkPageQuality({ only: ["link-text"] });
      expect(report.audits.map((a) => a.id)).toEqual(["link-text"]);
    });
  });
}
