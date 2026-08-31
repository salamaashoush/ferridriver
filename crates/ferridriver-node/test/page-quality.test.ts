// NAPI coverage for page.checkPageQuality(), the Lighthouse audits that
// score a page as it stands.
//
// Not a Playwright method. What these agree with is checked elsewhere
// (`just quality-diff`, and the recorded gate in ferridriver-perf); this
// is the binding surface: the option bag reaches Rust, the report comes
// back with its category, explanation and items intact.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { createServer, type Server } from "node:http";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND ? [process.env.FERRIDRIVER_BACKEND] : ["cdp-pipe"];

const SOUND =
  "<!doctype html><html lang='en'><head><title>t</title>" +
  "<meta name='description' content='A page with a description'></head>" +
  "<body><main><p><a href='https://example.com/pricing'>See our pricing</a></p></main></body></html>";

/// `http-status-code` and `is-crawlable` read the main document's own
/// response and fetch `/robots.txt`, neither of which a `setContent`
/// document has. `/status/<code>` answers with that status, `/blocked`
/// is the path robots.txt disallows, and anything else is 200.
async function serveFixtures(): Promise<{ origin: string; close: () => Promise<void> }> {
  const server: Server = createServer((req, res) => {
    const path = (req.url ?? "/").split("?")[0];
    if (path === "/robots.txt") {
      res.writeHead(200, { "content-type": "text/plain" });
      res.end("User-agent: *\nDisallow: /blocked\n");
      return;
    }
    const status = Number(path.match(/^\/status\/(\d+)$/)?.[1] ?? 200);
    const headers: Record<string, string | string[]> = { "content-type": "text/html" };
    if (path === "/noindex-header") headers["x-robots-tag"] = "noindex";
    res.writeHead(status, headers);
    res.end(SOUND);
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  const port = typeof address === "object" && address ? address.port : 0;
  return {
    origin: `http://127.0.0.1:${port}`,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

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

    it("reports every audit with a category on each", async () => {
      await page.setContent(SOUND);
      const report = await page.checkPageQuality();

      expect(report.audits.map((a) => a.id)).toEqual([
        "doctype",
        "meta-description",
        "canonical",
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

    it("scores the status and the crawlability of a page that was actually served", async () => {
      const fixtures = await serveFixtures();
      try {
        const audit = (report: Awaited<ReturnType<Page["checkPageQuality"]>>, id: string) =>
          report.audits.find((a) => a.id === id)!;

        await page.goto(`${fixtures.origin}/fine`);
        const sound = await page.checkPageQuality();
        expect(sound.audits.map((a) => a.id)).toContain("http-status-code");
        expect(audit(sound, "http-status-code").passed).toBe(true);
        expect(audit(sound, "is-crawlable").passed).toBe(true);

        await page.goto(`${fixtures.origin}/status/503`);
        const failed = audit(await page.checkPageQuality(), "http-status-code");
        expect(failed.passed).toBe(false);
        expect(failed.explanation).toBe("503");

        await page.goto(`${fixtures.origin}/noindex-header`);
        const header = audit(await page.checkPageQuality(), "is-crawlable");
        expect(header.passed).toBe(false);
        expect(header.items[0].snippet.toLowerCase()).toContain("x-robots-tag");

        await page.goto(`${fixtures.origin}/blocked`);
        const disallowed = audit(await page.checkPageQuality(), "is-crawlable");
        expect(disallowed.passed).toBe(false);
        expect(disallowed.items[0].snippet).toContain("/robots.txt");
      } finally {
        await fixtures.close();
      }
    });

    it("leaves both out for a document that arrived without a response", async () => {
      // A page of its own, and one that never navigates: `setContent`
      // keeps whatever URL the document already had, so a page that HAS
      // navigated still has a main-document response to be scored on.
      const fresh = await browser.newPage();
      try {
        await fresh.setContent(SOUND);
        const ids = (await fresh.checkPageQuality()).audits.map((a) => a.id);
        expect(ids).not.toContain("http-status-code");
        expect(ids).not.toContain("is-crawlable");
      } finally {
        await fresh.close();
      }
    });
  });
}
