/**
 * §3.12 NAPI parity tests: `getBy*` matchers and `RoleOptions.name`
 * accept a native JS `RegExp` in addition to literal strings.
 *
 * Proves the `#[napi(ts_type = "string | RegExp")]` + `JsRegExpLike`
 * prototype-chain trick round-trips a real RegExp end-to-end: NAPI
 * input → Rust `StringOrRegex::Regex { source, flags }` →
 * Playwright-native `internal:*` selector body (with `/source/flags`
 * literal) → injected engine regex matcher → DOM count.
 *
 * Runs on both CDP backends. WebKit uses the same verbatim Playwright
 * injected engine — the Rust-side integration suite
 * (`tests/backends_support/getby_regex.rs`) covers WebKit + BiDi.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

for (const backend of BACKENDS) {
  describe(`[${backend}] getBy* accept RegExp (§3.12)`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("getByText(/pattern/) matches regex", async () => {
      await page.goto(
        "data:text/html,<p>hello world</p><p>hello 42</p><p>hello 7</p><p>HELLO 9</p>",
        null,
      );
      const sensitive = await page.getByText(/hello \d+/).count();
      expect(sensitive).toBe(2);
      const insensitive = await page.getByText(/hello \d+/i).count();
      expect(insensitive).toBe(3);
    });

    it("getByText(literal) still works", async () => {
      await page.goto(
        "data:text/html,<p>Simple Text</p><p>Another</p>",
        null,
      );
      const count = await page.getByText("Simple Text").count();
      expect(count).toBe(1);
    });

    it("getByRole('button', { name: RegExp }) matches accessible name", async () => {
      await page.goto(
        "data:text/html,<button>Submit form</button><button>submit data</button><button>Cancel</button>",
        null,
      );
      const count = await page.getByRole("button", { name: /submit/i }).count();
      expect(count).toBe(2);
      const none = await page.getByRole("button", { name: /^no-match$/ }).count();
      expect(none).toBe(0);
    });

    it("getByPlaceholder(RegExp) matches attribute", async () => {
      await page.goto(
        "data:text/html,<input placeholder='Enter Email'><input placeholder='Your email'><input placeholder='Phone'>",
        null,
      );
      const count = await page.getByPlaceholder(/email/i).count();
      expect(count).toBe(2);
    });

    it("getByAltText(RegExp) matches alt", async () => {
      await page.goto(
        "data:text/html,<img alt='Photo 1' src=''/><img alt='Photo 42' src=''/><img alt='other' src=''/>",
        null,
      );
      const count = await page.getByAltText(/photo \d+/i).count();
      expect(count).toBe(2);
    });

    it("getByTitle(RegExp) matches title", async () => {
      await page.goto(
        "data:text/html,<span title='tooltip one'>a</span><span title='tooltip two'>b</span><span title='other'>c</span>",
        null,
      );
      const count = await page.getByTitle(/tooltip/i).count();
      expect(count).toBe(2);
    });

    it("getByLabel(RegExp) matches associated label", async () => {
      await page.goto(
        "data:text/html,<label for='e1'>Email Address</label><input id='e1'><label for='e2'>Work Email</label><input id='e2'><label for='p'>Phone</label><input id='p'>",
        null,
      );
      const count = await page.getByLabel(/email/i).count();
      expect(count).toBe(2);
    });

    it("getByTestId(RegExp) matches data-testid", async () => {
      await page.goto(
        "data:text/html,<div data-testid='card-1'>A</div><div data-testid='card-42'>B</div><div data-testid='other'>C</div>",
        null,
      );
      const count = await page.getByTestId(/card-\d+/).count();
      expect(count).toBe(2);
    });

    it("locator.getByText(RegExp) composes with a parent scope", async () => {
      await page.goto(
        "data:text/html,<div class='a'><span>hello 1</span><span>hello X</span></div><div class='b'><span>hello 99</span></div>",
        null,
      );
      const scoped = page.locator(".a");
      const count = await scoped.getByText(/hello \d+/).count();
      expect(count).toBe(1);
    });
  });
}
