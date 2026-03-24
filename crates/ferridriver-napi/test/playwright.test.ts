/**
 * Identical test suite to browser.test.ts but using Playwright.
 * Validates that ferridriver NAPI bindings behave the same as Playwright.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { chromium, type Browser, type Page } from "playwright";

describe("Browser", () => {
  it("launches and closes", async () => {
    const browser = await chromium.launch();
    expect(browser).toBeDefined();
    await browser.close();
  });

  it("creates a new page", async () => {
    const browser = await chromium.launch();
    const page = await browser.newPage();
    expect(page).toBeDefined();
    await browser.close();
  });
});

describe("Page navigation", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
    await page.goto("https://example.com");
  });

  afterAll(async () => {
    await browser.close();
  });

  it("navigates to a URL", async () => {
    const url = page.url();
    expect(url).toContain("example.com");
  });

  it("gets the page title", async () => {
    const title = await page.title();
    expect(title).toContain("Example Domain");
  });

  it("gets page content", async () => {
    const html = await page.content();
    expect(html).toContain("<h1>Example Domain</h1>");
  });

  it("navigates with goto", async () => {
    await page.goto("https://example.com");
    const url = page.url();
    expect(url).toContain("example.com");
  });

  it("waits for load state", async () => {
    await page.goto("https://example.com");
    await page.waitForLoadState();
    const title = await page.title();
    expect(title).toContain("Example Domain");
  });
});

describe("Page evaluation", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
    await page.goto("https://example.com");
  });

  afterAll(async () => {
    await browser.close();
  });

  it("evaluates JS and returns value", async () => {
    const result = await page.evaluate("1 + 1");
    expect(result).toBe(2);
  });

  it("evaluates JS and returns string", async () => {
    const result = await page.evaluate("document.title");
    expect(result).toContain("Example Domain");
  });

  it("evaluates JS and returns null", async () => {
    const result = await page.evaluate("null");
    expect(result).toBeNull();
  });

  it("evaluates JS and returns object", async () => {
    const result = await page.evaluate("({a: 1, b: 'hello'})");
    expect(result).toEqual({ a: 1, b: "hello" });
  });
});

describe("Page selectors", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
    await page.goto("https://example.com");
  });

  afterAll(async () => {
    await browser.close();
  });

  it("finds element text with selector", async () => {
    const text = await page.innerText("h1");
    expect(text).toBe("Example Domain");
  });

  it("gets innerHTML", async () => {
    const html = await page.innerHTML("h1");
    expect(html).toBe("Example Domain");
  });

  it("checks element visibility", async () => {
    const visible = await page.isVisible("h1");
    expect(visible).toBe(true);
  });

  it("checks element is not hidden", async () => {
    const hidden = await page.isHidden("h1");
    expect(hidden).toBe(false);
  });

  it("returns hidden for nonexistent selector (Playwright behavior)", async () => {
    const hidden = await page.isHidden("#does-not-exist");
    expect(hidden).toBe(true);
  });

  it("returns not visible for nonexistent selector", async () => {
    const visible = await page.isVisible("#does-not-exist");
    expect(visible).toBe(false);
  });
});

describe("Locator", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
    await page.goto("https://example.com");
  });

  afterAll(async () => {
    await browser.close();
  });

  it("gets text content via locator", async () => {
    const loc = page.locator("h1");
    const text = await loc.textContent();
    expect(text).toBe("Example Domain");
  });

  it("gets inner text via locator", async () => {
    const text = await page.locator("h1").innerText();
    expect(text).toBe("Example Domain");
  });

  it("checks visibility via locator", async () => {
    const visible = await page.locator("h1").isVisible();
    expect(visible).toBe(true);
  });

  it("counts matching elements", async () => {
    const count = await page.locator("p").count();
    expect(count).toBeGreaterThan(0);
  });

  it("gets bounding box", async () => {
    const box_ = await page.locator("h1").boundingBox();
    expect(box_).not.toBeNull();
    expect(box_!.width).toBeGreaterThan(0);
    expect(box_!.height).toBeGreaterThan(0);
  });

  it("gets all text contents", async () => {
    const texts = await page.locator("p").allTextContents();
    expect(texts.length).toBeGreaterThan(0);
  });

  it("creates first/last/nth locators", async () => {
    const first = await page.locator("p").first().textContent();
    expect(first).toBeDefined();
  });
});

describe("Page screenshots", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
    await page.goto("https://example.com");
  });

  afterAll(async () => {
    await browser.close();
  });

  it("takes a page screenshot", async () => {
    const buf = await page.screenshot();
    expect(buf.length).toBeGreaterThan(0);
    // PNG magic bytes
    expect(buf[0]).toBe(0x89);
    expect(buf[1]).toBe(0x50);
    expect(buf[2]).toBe(0x4e);
    expect(buf[3]).toBe(0x47);
  });

  it("takes a full page screenshot", async () => {
    const buf = await page.screenshot({ fullPage: true });
    expect(buf.length).toBeGreaterThan(0);
  });

  it("takes an element screenshot", async () => {
    const buf = await page.locator("h1").screenshot();
    expect(buf.length).toBeGreaterThan(0);
  });
});

describe("Page viewport and emulation", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
    await page.goto("https://example.com");
  });

  afterAll(async () => {
    await browser.close();
  });

  it("sets viewport size", async () => {
    await page.setViewportSize({ width: 800, height: 600 });
    const width = await page.evaluate("window.innerWidth");
    expect(width).toBe(800);
  });
});

describe("Page cookies", () => {
  let browser: Browser;
  let context: Awaited<ReturnType<Browser["newContext"]>>;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    context = await browser.newContext();
    page = await context.newPage();
    await page.goto("https://example.com");
  });

  afterAll(async () => {
    await browser.close();
  });

  it("sets and gets a cookie", async () => {
    await context.addCookies([
      {
        name: "test",
        value: "hello",
        domain: ".example.com",
        path: "/",
        secure: true,
        httpOnly: false,
      },
    ]);
    const cookies = await context.cookies();
    const found = cookies.find((c) => c.name === "test");
    expect(found).toBeDefined();
    expect(found!.value).toBe("hello");
  });

  it("deletes a specific cookie by name and domain", async () => {
    await context.clearCookies({ name: "test", domain: ".example.com" });
    const cookies = await context.cookies();
    const found = cookies.find((c) => c.name === "test");
    expect(found).toBeUndefined();
  });

  it("clears all cookies", async () => {
    await context.addCookies([
      {
        name: "a",
        value: "1",
        domain: ".example.com",
        path: "/",
        secure: false,
        httpOnly: false,
      },
    ]);
    await context.clearCookies();
    const cookies = await context.cookies();
    expect(cookies.length).toBe(0);
  });
});

describe("Page set content", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("sets HTML content and reads it back", async () => {
    await page.setContent("<html><body><h1>Hello</h1></body></html>");
    const text = await page.innerText("h1");
    expect(text).toBe("Hello");
  });

  it("interacts with form elements", async () => {
    await page.setContent(`
      <form>
        <input type="text" id="name" />
        <input type="checkbox" id="agree" />
        <select id="color">
          <option value="red">Red</option>
          <option value="blue">Blue</option>
        </select>
      </form>
    `);

    await page.fill("#name", "Ferridriver");
    const value = await page.inputValue("#name");
    expect(value).toBe("Ferridriver");

    await page.check("#agree");
    const checked = await page.isChecked("#agree");
    expect(checked).toBe(true);

    await page.uncheck("#agree");
    const unchecked = await page.isChecked("#agree");
    expect(unchecked).toBe(false);
  });
});

describe("Locator actions on dynamic content", () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await chromium.launch();
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("clicks a button and verifies effect", async () => {
    await page.setContent(`
      <button id="btn" onclick="document.getElementById('result').textContent = 'clicked'">Click me</button>
      <div id="result"></div>
    `);
    await page.locator("#btn").click();
    const text = await page.locator("#result").innerText();
    expect(text).toBe("clicked");
  });

  it("fills an input via locator", async () => {
    await page.setContent('<input id="input" type="text" />');
    const loc = page.locator("#input");
    await loc.fill("test value");
    const val = await loc.inputValue();
    expect(val).toBe("test value");
  });

  it("clears an input via locator", async () => {
    await page.setContent('<input id="input" type="text" value="hello" />');
    const loc = page.locator("#input");
    await loc.clear();
    const val = await loc.inputValue();
    expect(val).toBe("");
  });

  it("hovers an element", async () => {
    await page.setContent(`
      <style>div:hover { color: red; }</style>
      <div id="hoverable">Hover me</div>
    `);
    await page.locator("#hoverable").hover();
  });

  it("focuses and blurs an element", async () => {
    await page.setContent('<input id="input" type="text" />');
    const loc = page.locator("#input");
    await loc.focus();
    const focused = await page.evaluate(
      "document.activeElement?.id || ''"
    );
    expect(focused).toBe("input");

    await loc.blur();
    const blurred = await page.evaluate(
      "document.activeElement?.tagName || ''"
    );
    expect((blurred as string).toLowerCase()).toBe("body");
  });

  it("dispatches a custom event", async () => {
    await page.setContent(`
      <div id="target"></div>
      <script>
        document.getElementById('target').addEventListener('custom', () => {
          document.getElementById('target').textContent = 'event fired';
        });
      </script>
    `);
    await page.locator("#target").dispatchEvent("custom");
    const text = await page.locator("#target").innerText();
    expect(text).toBe("event fired");
  });

  it("waits for an element to appear", async () => {
    await page.setContent(`
      <script>setTimeout(() => {
        const el = document.createElement('div');
        el.id = 'delayed';
        el.textContent = 'appeared';
        document.body.appendChild(el);
      }, 200);</script>
    `);
    await page.locator("#delayed").waitFor({ state: "attached", timeout: 5000 });
    const text = await page.locator("#delayed").innerText();
    expect(text).toBe("appeared");
  });
});
