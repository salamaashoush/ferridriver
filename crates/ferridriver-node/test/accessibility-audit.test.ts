// NAPI coverage for page.checkAccessibility(), which runs axe-core in
// the page. Not a Playwright method — Playwright's `page.accessibility`
// is the accessibility TREE, a different question.
//
// Needs axe-core on disk: `ferridriver install axe`. The last test
// pins the message you get when it is missing, because "it threw" is
// not a useful thing for a first-time user to be told.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

// An image with no alt text and a page with no title or lang: three
// rules that axe fires on reliably, at three different impacts.
const BROKEN_PAGE = "<html><body><img src='data:,x'><p>text</p></body></html>";

for (const backend of BACKENDS) {
  describe(`accessibility audit [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("reports the rules that failed, the elements, and the engine", async () => {
      await page.setContent(BROKEN_PAGE);
      const report = await page.checkAccessibility();

      expect(report.engine).toMatch(/^axe-core\/\d+\.\d+\.\d+$/);
      const failed = report.violations.map((v) => v.id);
      expect(failed).toContain("image-alt");
      expect(failed).toContain("html-has-lang");

      const imageAlt = report.violations.find((v) => v.id === "image-alt")!;
      expect(imageAlt.impact).toBe("critical");
      expect(imageAlt.helpUrl).toContain("dequeuniversity.com");
      // The element and the reason, which is the part anyone acts on.
      expect(imageAlt.nodes.length).toBe(1);
      expect(imageAlt.nodes[0].target[0]).toContain("img");
      expect(imageAlt.nodes[0].failureSummary.length).toBeGreaterThan(0);
    });

    it("passes and violations are both reported, so a clean page is distinguishable from an unrun one", async () => {
      await page.setContent("<html lang='en'><head><title>Fine</title></head><body><main><p>ok</p></main></body></html>");
      const report = await page.checkAccessibility();
      expect(report.passes.length).toBeGreaterThan(0);
      expect(report.violations.map((v) => v.id)).not.toContain("html-has-lang");
    });

    it("runs the rules axe hides unless asked, which Lighthouse asks for", async () => {
      // `target-size` ships `enabled: false` and `td-has-header` is
      // tagged `experimental`, so axe's defaults drop both. Lighthouse
      // enables them, so an audit claiming to cover what Lighthouse
      // covers has to reach them too.
      await page.setContent("<html lang='en'><head><title>t</title></head><body><p>x</p></body></html>");
      const report = await page.checkAccessibility();
      const evaluated = new Set(
        [...report.violations, ...report.passes, ...report.incomplete, ...report.inapplicable].map((r) => r.id),
      );
      for (const rule of ["target-size", "td-has-header", "table-fake-caption", "identical-links-same-purpose"]) {
        expect(evaluated.has(rule)).toBe(true);
      }
    });

    it("tags narrow which rules run", async () => {
      await page.setContent(BROKEN_PAGE);
      const all = await page.checkAccessibility();
      const wcag2a = await page.checkAccessibility({ tags: ["wcag2a"] });

      // Every rule that survived the filter carries the tag, and the
      // filter actually removed something.
      for (const rule of wcag2a.violations) expect(rule.tags).toContain("wcag2a");
      const allIds = new Set(all.violations.map((v) => v.id));
      const taggedIds = new Set(wcag2a.violations.map((v) => v.id));
      expect(taggedIds.size).toBeLessThan(allIds.size);
      for (const id of taggedIds) expect(allIds.has(id)).toBe(true);
    });

    it("include narrows the audit to a subtree", async () => {
      await page.setContent(
        "<html lang='en'><head><title>t</title></head><body>" +
          "<div id='left'><img src='data:,a'></div><div id='right'><img src='data:,b'></div>" +
          "</body></html>",
      );
      const whole = await page.checkAccessibility();
      const left = await page.checkAccessibility({ include: ["#left"] });

      const count = (r: Awaited<ReturnType<Page["checkAccessibility"]>>) =>
        r.violations.find((v) => v.id === "image-alt")?.nodes.length ?? 0;
      expect(count(whole)).toBe(2);
      expect(count(left)).toBe(1);
    });
  });
}
