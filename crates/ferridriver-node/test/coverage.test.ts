// page.coverage through the binding (Chromium only, as in Playwright).
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw"];

const FILES: Record<string, [string, string]> = {
  "/": ["text/html", '<link rel="stylesheet" href="/sheet.css"><script src="/script.js"></script><div class="used">x</div>'],
  "/script.js": ["text/javascript", "function used() { return 1; }\nfunction unused() { return 2; }\nused();\n"],
  "/sheet.css": ["text/css", ".used { color: red; }\n.unused { color: blue; }\n"],
};

for (const backend of BACKENDS) {
  describe(`[${backend}] coverage`, () => {
    let browser: Browser;
    let server: ReturnType<typeof Bun.serve>;
    let origin: string;

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        fetch(request) {
          const [type, body] = FILES[new URL(request.url).pathname] ?? ["text/plain", "missing"];
          return new Response(body, { headers: { "content-type": type } });
        },
      });
      origin = `http://127.0.0.1:${server.port}`;
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    it("reports JavaScript functions and CSS rules in use", async () => {
      const page = await browser.newPage();
      await page.coverage.startJSCoverage();
      await page.coverage.startCSSCoverage();
      await page.goto(`${origin}/`);
      const scripts = await page.coverage.stopJSCoverage();
      const sheets = await page.coverage.stopCSSCoverage();
      const script = scripts.find((entry) => entry.url.endsWith("/script.js"))!;
      const unused = script.functions.find((fn) => fn.functionName === "unused")!;
      expect(unused.ranges[0]!.count).toBe(0);
      const sheet = sheets.find((entry) => entry.url.endsWith("/sheet.css"))!;
      const used = sheet.ranges.map((range) => sheet.text!.slice(range.start, range.end)).join("");
      expect(used).toContain(".used");
      expect(used).not.toContain(".unused");
      await page.close();
    });
  });
}
