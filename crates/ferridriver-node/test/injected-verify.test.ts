/**
 * Verify all window.__fd functions ported from Playwright work correctly.
 * Tests every function in the injected script against real browser behavior.
 */
import { test, expect, describe, beforeAll, afterAll } from "bun:test";
import { Browser } from "../index.js";

// When FERRIDRIVER_BACKEND is set, run only that backend for parallel execution.
const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND] as const
  : ["cdp-pipe", "cdp-raw"] as const;

for (const backend of BACKENDS) {
  describe(`[${backend}] injected script verification`, () => {
    let browser: any;
    let page: any;

    const HTML = `<!DOCTYPE html>
<html><head><title>Verify Page</title></head>
<body>
  <h1 id="heading">Hello World</h1>
  <p class="intro">This is a test paragraph with <strong>bold</strong> text.</p>
  <div id="hidden" style="display:none">Hidden content</div>
  <div id="invisible" style="visibility:hidden">Invisible content</div>
  <div id="zero-size" style="width:0;height:0;overflow:hidden">Zero size</div>
  <input id="text-input" type="text" placeholder="Enter name" value="initial" />
  <input id="disabled-input" type="text" disabled value="disabled" />
  <input id="readonly-input" type="text" readonly value="readonly" />
  <input id="checkbox" type="checkbox" />
  <input id="checked-box" type="checkbox" checked />
  <input id="file-input" type="file" />
  <select id="dropdown">
    <option value="a">Option A</option>
    <option value="b">Option B</option>
    <option value="c" selected>Option C</option>
  </select>
  <button id="btn" onclick="document.getElementById('result').textContent='clicked'">Click Me</button>
  <div id="result"></div>
  <fieldset disabled>
    <input id="fieldset-disabled" type="text" value="in disabled fieldset" />
  </fieldset>
  <div role="button" aria-disabled="true" id="aria-disabled-btn">Aria Disabled</div>
  <ul id="list">
    <li>Item 1</li>
    <li>Item 2</li>
    <li>Item 3</li>
  </ul>
  <a href="https://example.com" id="link">Example Link</a>
  <div data-testid="test-element">Test ID Element</div>
  <label for="text-input">Name Label</label>
  <div style="height:2000px"></div>
  <div id="bottom">Bottom content</div>
</body></html>`;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
      // Use data URL instead of setContent to ensure addScriptToEvaluateOnNewDocument fires
      const dataUrl = `data:text/html,${encodeURIComponent(HTML)}`;
      await page.goto(dataUrl);
      // Trigger selector engine injection by performing a locator query
      await page.waitForSelector("#heading");
    });

    afterAll(async () => {
      await browser.close();
    });

    // ── Selector API ──

    test("sel() returns JSON with matched elements", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"css",body:"h1"}])');
      const parsed = JSON.parse(result);
      expect(Array.isArray(parsed)).toBe(true);
      expect(parsed.length).toBe(1);
      expect(parsed[0].tag).toBe("h1");
      expect(parsed[0].text).toContain("Hello World");
    });

    test("sel() with text engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"text",body:"Hello World"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBeGreaterThanOrEqual(1);
    });

    test("sel() with role engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"role",body:"button"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBeGreaterThanOrEqual(1); // at least the <button>
    });

    test("sel() with testid engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"testid",body:"test-element"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBe(1);
      expect(parsed[0].text).toContain("Test ID Element");
    });

    test("sel() with id engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"id",body:"heading"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBe(1);
      expect(parsed[0].tag).toBe("h1");
    });

    test("sel() with xpath engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"xpath",body:"//h1"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBe(1);
      expect(parsed[0].tag).toBe("h1");
    });

    test("sel() with chained selectors (>>)", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"css",body:"#list"},{engine:"css",body:"li"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBe(3);
    });

    test("sel() with nth engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"css",body:"li"},{engine:"nth",body:"1"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBe(1);
      expect(parsed[0].text).toContain("Item 2");
    });

    test("sel() with visible engine", async () => {
      // heading is visible
      const result1 = await page.evaluate('window.__fd.sel([{engine:"css",body:"#heading"},{engine:"visible",body:"true"}])');
      const p1 = JSON.parse(result1);
      expect(p1.length).toBe(1);

      // hidden div should not match visible=true
      const result2 = await page.evaluate('window.__fd.sel([{engine:"css",body:"#hidden"},{engine:"visible",body:"true"}])');
      const p2 = JSON.parse(result2);
      expect(p2.length).toBe(0);
    });

    test("sel() with has engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"css",body:"body"},{engine:"has",body:"h1"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBe(1);
    });

    test("sel() with has-text engine", async () => {
      const result = await page.evaluate('window.__fd.sel([{engine:"css",body:"p"},{engine:"has-text",body:"bold"}])');
      const parsed = JSON.parse(result);
      expect(parsed.length).toBe(1);
    });

    test("selOne() returns single element or null", async () => {
      const result = await page.evaluate('window.__fd.selOne([{engine:"css",body:"h1"}]) ? "found" : "null"');
      expect(result).toBe("found");

      const result2 = await page.evaluate('window.__fd.selOne([{engine:"css",body:"h99"}]) ? "found" : "null"');
      expect(result2).toBe("null");
    });

    test("selAll() returns array of elements", async () => {
      const count = await page.evaluate('window.__fd.selAll([{engine:"css",body:"li"}]).length');
      expect(count).toBe(3);
    });

    test("selCount() returns count", async () => {
      const count = await page.evaluate('window.__fd.selCount([{engine:"css",body:"li"}])');
      expect(count).toBe(3);
    });

    test("_exec() returns array of elements", async () => {
      const count = await page.evaluate('window.__fd._exec([{engine:"css",body:"li"}], document).length');
      expect(count).toBe(3);
    });

    // ── Visibility / Actionability ──

    test("isVisible() checks element visibility", async () => {
      expect(await page.evaluate('window.__fd.isVisible(document.getElementById("heading"))')).toBe(true);
      expect(await page.evaluate('window.__fd.isVisible(document.getElementById("hidden"))')).toBe(false);
      expect(await page.evaluate('window.__fd.isVisible(document.getElementById("invisible"))')).toBe(false);
    });

    test("elementState() checks visible/hidden", async () => {
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("heading"), "visible")')).toBe(true);
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("heading"), "hidden")')).toBe(false);
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("hidden"), "hidden")')).toBe(true);
    });

    test("elementState() checks enabled/disabled", async () => {
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("text-input"), "enabled")')).toBe(true);
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("disabled-input"), "disabled")')).toBe(true);
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("disabled-input"), "enabled")')).toBe(false);
    });

    test("elementState() checks editable", async () => {
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("text-input"), "editable")')).toBe(true);
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("disabled-input"), "editable")')).toBe(false);
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("readonly-input"), "editable")')).toBe(false);
    });

    test("elementState() checks checked/unchecked", async () => {
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("checked-box"), "checked")')).toBe(true);
      expect(await page.evaluate('window.__fd.elementState(document.getElementById("checkbox"), "unchecked")')).toBe(true);
    });

    test("checkElementStates() returns 'done' when all pass", async () => {
      expect(await page.evaluate('window.__fd.checkElementStates(document.getElementById("text-input"), ["visible", "enabled"])')).toBe("done");
    });

    test("checkElementStates() returns error on failure", async () => {
      const result = await page.evaluate('window.__fd.checkElementStates(document.getElementById("hidden"), ["visible"])');
      expect(result).toContain("error:");
    });

    test("isActionable() returns true for actionable elements", async () => {
      const r = await page.evaluate('JSON.stringify(window.__fd.isActionable(document.getElementById("btn")))');
      const parsed = JSON.parse(r);
      expect(parsed.actionable).toBe(true);
    });

    test("isActionable() returns false for hidden elements", async () => {
      const r = await page.evaluate('JSON.stringify(window.__fd.isActionable(document.getElementById("hidden")))');
      const parsed = JSON.parse(r);
      expect(parsed.actionable).toBe(false);
      expect(parsed.reason).toBe("notvisible");
    });

    test("isActionable() returns false for disabled elements", async () => {
      const r = await page.evaluate('JSON.stringify(window.__fd.isActionable(document.getElementById("disabled-input")))');
      const parsed = JSON.parse(r);
      expect(parsed.actionable).toBe(false);
      expect(parsed.reason).toBe("disabled");
    });

    test("getAriaDisabled() detects fieldset-disabled", async () => {
      expect(await page.evaluate('window.__fd.getAriaDisabled(document.getElementById("fieldset-disabled"))')).toBe(true);
    });

    test("getAriaDisabled() detects aria-disabled", async () => {
      expect(await page.evaluate('window.__fd.getAriaDisabled(document.getElementById("aria-disabled-btn"))')).toBe(true);
    });

    // ── Click Guard ──

    test("clickGuard() detects select elements", async () => {
      expect(await page.evaluate('window.__fd.clickGuard(document.getElementById("dropdown"))')).toBe("select");
    });

    test("clickGuard() detects file inputs", async () => {
      expect(await page.evaluate('window.__fd.clickGuard(document.getElementById("file-input"))')).toBe("file");
    });

    test("clickGuard() returns empty for normal elements", async () => {
      expect(await page.evaluate('window.__fd.clickGuard(document.getElementById("btn"))')).toBe("");
    });

    // ── ARIA ──

    test("getAriaRole() returns correct roles", async () => {
      expect(await page.evaluate('window.__fd.getAriaRole(document.getElementById("btn"))')).toBe("button");
      expect(await page.evaluate('window.__fd.getAriaRole(document.getElementById("link"))')).toBe("link");
      expect(await page.evaluate('window.__fd.getAriaRole(document.getElementById("heading"))')).toBe("heading");
      expect(await page.evaluate('window.__fd.getAriaRole(document.getElementById("checkbox"))')).toBe("checkbox");
    });

    test("getAccessibleName() returns element names", async () => {
      const name = await page.evaluate('window.__fd.getAccessibleName(document.getElementById("btn"))');
      expect(name).toContain("Click Me");
    });

    // ── Actions ──

    test("clearAndDispatch() clears input value", async () => {
      await page.evaluate('window.__fd.clearAndDispatch(document.getElementById("text-input"))');
      const val = await page.evaluate('document.getElementById("text-input").value');
      expect(val).toBe("");
    });

    test("clearAndDispatch() sets new value when provided", async () => {
      await page.evaluate('window.__fd.clearAndDispatch(document.getElementById("text-input"), "new value")');
      const val = await page.evaluate('document.getElementById("text-input").value');
      expect(val).toBe("new value");
    });

    test("selectOption() selects by value", async () => {
      const r = await page.evaluate('JSON.stringify(window.__fd.selectOption(document.getElementById("dropdown"), "b"))');
      const parsed = JSON.parse(r);
      expect(parsed.selected).toBe(true);
      expect(parsed.value).toBe("b");
      expect(await page.evaluate('document.getElementById("dropdown").value')).toBe("b");
    });

    test("selectOption() selects by text", async () => {
      const r = await page.evaluate('JSON.stringify(window.__fd.selectOption(document.getElementById("dropdown"), "Option A"))');
      const parsed = JSON.parse(r);
      expect(parsed.selected).toBe(true);
      expect(parsed.value).toBe("a");
    });

    test("selectOption() returns error for missing option", async () => {
      const r = await page.evaluate('JSON.stringify(window.__fd.selectOption(document.getElementById("dropdown"), "nonexistent"))');
      const parsed = JSON.parse(r);
      expect(parsed.selected).toBe(false);
      expect(parsed.error).toBeDefined();
    });

    test("getOptions() returns all options", async () => {
      const r = await page.evaluate('JSON.stringify(window.__fd.getOptions(document.getElementById("dropdown")))');
      const parsed = JSON.parse(r);
      expect(parsed.options.length).toBe(3);
      expect(parsed.options[0].text).toBe("Option A");
      expect(parsed.options[0].value).toBe("a");
    });

    // ── Utilities ──

    test("searchPage() finds text matches", async () => {
      // searchPage returns a JSON string already, so use evaluateStr without extra JSON.stringify
      const r = await page.evaluate('window.__fd.searchPage("Hello", false, false, 10, "", 10)');
      const parsed = JSON.parse(r);
      expect(parsed.total).toBeGreaterThanOrEqual(1);
      expect(parsed.matches[0].match_text).toBe("Hello");
    });

    test("searchPage() supports regex", async () => {
      const r = await page.evaluate('window.__fd.searchPage("Item \\\\d+", true, false, 10, "", 10)');
      const parsed = JSON.parse(r);
      expect(parsed.total).toBe(3);
    });

    test("findElementsCSS() finds elements", async () => {
      const r = await page.evaluate('window.__fd.findElementsCSS("li", ["id"], 10, true)');
      const parsed = JSON.parse(r);
      expect(parsed.length).toBe(3);
      expect(parsed[0].tag).toBe("li");
    });

    test("scrollInfo() returns scroll data", async () => {
      const r = await page.evaluate('window.__fd.scrollInfo()');
      const parsed = typeof r === "string" ? JSON.parse(r) : r;
      expect(parsed.viewportHeight).toBeGreaterThan(0);
      expect(parsed.scrollHeight).toBeGreaterThan(0);
    });

    test("suggestSelectors() returns suggestions", async () => {
      const r = await page.evaluate('window.__fd.suggestSelectors()');
      const parsed = JSON.parse(r);
      expect(parsed.ids.length).toBeGreaterThan(0);
      expect(parsed.ids).toContain("heading");
    });

    test("consoleErrors() installs and counts", async () => {
      const count = await page.evaluate('window.__fd.consoleErrors()');
      expect(typeof count).toBe("number");
    });

    test("extractMarkdown() converts page to markdown", async () => {
      const md = await page.evaluate('window.__fd.extractMarkdown()');
      expect(md).toContain("# Hello World");
      expect(md).toContain("**bold**");
    });

    test("allElements() returns all page elements", async () => {
      const count = await page.evaluate('window.__fd.allElements().length');
      expect(count).toBeGreaterThan(10);
    });
  });
}
