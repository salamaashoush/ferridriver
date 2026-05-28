/**
 * keyboard.type({ namedKeys }) parity with Playwright.
 *
 * Playwright `keyboard.type(text, { namedKeys: true })` parses `{Name}` /
 * `{Mod+Key}` sequences out of the text and dispatches them as key presses
 * (same format as `keyboard.press`); `{{` types a literal `{`. Without the
 * option the braces are typed verbatim. These tests drive a real backend and
 * observe the DOM-side effect (newline, selection replacement, literal brace)
 * that only occurs when the option took effect.
 *
 * Mirrors /tmp/playwright/packages/playwright-core/src/server/input.ts
 * (parseNamedKeys + Keyboard.type).
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";
import { createServer, type Server } from "node:http";

let testServer: Server;
let testUrl: string;

const PAGE = `<!DOCTYPE html>
<html>
  <head><title>keyboard.type namedKeys fixture</title></head>
  <body>
    <textarea id="area"></textarea>
    <input id="seeded" value="seed" />
  </body>
</html>`;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(PAGE);
  });
  await new Promise<void>((resolve) => {
    testServer.listen(0, "127.0.0.1", () => {
      const addr = testServer.address() as any;
      testUrl = `http://127.0.0.1:${addr.port}`;
      resolve();
    });
  });
});

afterAll(() => {
  testServer?.close();
});

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (() => {
      const b = ["cdp-pipe", "cdp-raw"];
      if (process.platform === "darwin") b.push("webkit");
      return b;
    })();

for (const backend of BACKENDS) {
  describe(`[${backend}] keyboard.type namedKeys`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("namedKeys=true presses {Enter} as a newline", async () => {
      await page.locator("#area").focus();
      await page.locator("#area").fill("");
      await page.keyboard.type("Hello{Enter}World", { namedKeys: true });
      expect(await page.inputValue("#area")).toBe("Hello\nWorld");
    });

    it("default (namedKeys unset) types braces verbatim", async () => {
      await page.locator("#area").focus();
      await page.locator("#area").fill("");
      await page.keyboard.type("A{Enter}B");
      expect(await page.inputValue("#area")).toBe("A{Enter}B");
    });

    it("double-brace types a literal brace", async () => {
      await page.locator("#area").focus();
      await page.locator("#area").fill("");
      await page.keyboard.type("a{{b", { namedKeys: true });
      expect(await page.inputValue("#area")).toBe("a{b");
    });

    it("{Backspace} presses a real key that edits the value", async () => {
      await page.locator("#area").focus();
      await page.locator("#area").fill("");
      await page.keyboard.type("abc{Backspace}d", { namedKeys: true });
      expect(await page.inputValue("#area")).toBe("abd");
    });

    it("{Control+a} dispatches a keydown carrying the modifier", async () => {
      await page.locator("#seeded").focus();
      await page.evaluate(
        `(() => { window.__ctrlA = ''; document.getElementById('seeded').addEventListener('keydown', e => { if (e.key === 'a') window.__ctrlA += 'ctrl=' + e.ctrlKey + ';'; }); })()`,
      );
      await page.keyboard.type("{Control+a}", { namedKeys: true });
      const log = await page.evaluate(`window.__ctrlA`);
      expect(log).toContain("ctrl=true");
    });
  });
}
