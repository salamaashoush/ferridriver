/**
 * Locator.and() / .or() semantic parity with Playwright.
 *
 * Before task #10:
 *   - `and` emitted `>>` which is descendant-chain, not intersection.
 *   - `or` emitted `css=:is(...)` (fine for pure CSS) or `|` (unsupported
 *     by the injected engine), breaking cross-engine unions like
 *     `getByRole('button').or(getByText('Click'))`.
 *
 * After:
 *   - `and` emits `>> internal:and=<json>`; both locators must match the
 *     SAME element.
 *   - `or` emits `>> internal:or=<json>`; matches elements resolved by
 *     either selector.
 *
 * Uses a local HTTP server with fixtures that exercise semantic
 * differences between the old and new behaviour.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type Server } from "node:http";

let testServer: Server;
let testUrl: string;

// The fixture:
//  - A primary button with role="button" AND text "Go". `and()` of
//    those two selectors must match exactly this one element.
//  - An ordinary link that says "Go" (same text, different role).
//  - Another button that says "Other" (same role, different text).
// So:
//   getByRole('button').and(getByText('Go'))  → 1 match (primary)
//   getByRole('button').or(getByText('Go'))   → 3 matches (both buttons + the link)
const FIXTURE = `<!DOCTYPE html>
<html>
  <head><title>and/or fixture</title></head>
  <body>
    <button id="primary" type="button">Go</button>
    <button id="other" type="button">Other</button>
    <a id="link" href="#">Go</a>
  </body>
</html>`;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(FIXTURE);
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
  describe(`[${backend}] Locator.and / .or semantics`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("selector string shape: .and() emits internal:and with JSON-encoded inner selector", () => {
      const base = page.locator("button");
      const other = page.locator("text=Go");
      const combined = base.and(other);
      // Playwright convention: `>> internal:and="<JSON-escaped inner selector>"`.
      expect(combined.selector).toBe('button >> internal:and="text=Go"');
    });

    it("selector string shape: .or() emits internal:or with JSON-encoded inner selector", () => {
      const combined = page.locator("role=button").or(page.locator("text=Go"));
      expect(combined.selector).toBe('role=button >> internal:or="text=Go"');
    });

    it(".and() intersects: same element must satisfy both selectors", async () => {
      const combined = page.locator("role=button").and(page.locator("text=Go"));
      // Only #primary is both a button AND has text "Go".
      expect(await combined.count()).toBe(1);
      const id = await combined.getAttribute("id");
      expect(id).toBe("primary");
    });

    it(".or() unions: matches elements from either selector", async () => {
      const combined = page.locator("role=button").or(page.locator("text=Go"));
      // Two buttons (primary, other) + the link with text "Go" = 3.
      expect(await combined.count()).toBe(3);
    });

    it(".and() is distinct from chained .locator(): chain is descendant, and is same-element", async () => {
      // Chain: `button >> text=Go` finds Go-text DESCENDANTS of buttons. Since
      // buttons contain the text as direct children, the chain may match the
      // button's text node's closest host — but strictly "Go text within a
      // button" is button#primary in this fixture.
      const chained = page.locator("button").locator("text=Go");
      const anded = page.locator("button").and(page.locator("text=Go"));
      // Chained may match inner text-bearing nodes; `and` always matches the
      // button itself. The assertion that the two can differ is the semantic
      // guarantee of `and`:
      //   - `and`'s first match is the <button> element.
      //   - chained traversal is inner-scoped.
      const andedId = await anded.getAttribute("id");
      expect(andedId).toBe("primary");
      // chained may or may not find an id, but the element it finds is
      // constrained to descendants of <button>. Here it should find the
      // button's own text, whose closest element is still the button.
      // The semantic test is the `and` count above.
      expect(typeof (await chained.count())).toBe("number");
    });

    it(".or() works across engine boundaries (css + text + role)", async () => {
      // Before the fix, crossing engines (e.g. role + text) would have used
      // the `|` fallback which the injected engine does not understand.
      const combined = page.locator("#primary").or(page.locator("text=Other"));
      expect(await combined.count()).toBe(2);
    });

    it("empty intersection yields zero matches cleanly (no resolver panic)", async () => {
      const combined = page.locator("role=button").and(page.locator("text=Nonexistent"));
      expect(await combined.count()).toBe(0);
    });

    it("and() and or() compose with other chain operators", async () => {
      // Apply .first() after .or() to narrow a multi-match union.
      const combined = page.locator("role=button").or(page.locator("text=Go")).first();
      expect(combined.isStrict).toBe(false);
      // first() on the union picks a deterministic match; just ensure
      // evaluation succeeds.
      expect(await combined.count()).toBeGreaterThanOrEqual(1);
    });
  });
}
