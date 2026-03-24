import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";

const BACKENDS: string[] = ["cdp-ws", "cdp-pipe", "cdp-raw"];
if (process.platform === "darwin") {
  BACKENDS.push("webkit");
}

describe("Browser (general)", () => {
  it("rejects unknown backend", async () => {
    expect(Browser.launch({ backend: "unknown" })).rejects.toThrow(
      "Unknown backend"
    );
  });
});

for (const backend of BACKENDS) {
  describe(`[${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPageWithUrl("https://example.com");
    });

    afterAll(async () => {
      await browser.close();
    });

    // ── Navigation ────────────────────────────────────────────────────

    it("navigates to a URL", async () => {
      const url = await page.url();
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

    it("extracts markdown", async () => {
      const md = await page.markdown();
      expect(md).toContain("Example Domain");
    });

    it("navigates with goto", async () => {
      await page.goto("https://example.com");
      const url = await page.url();
      expect(url).toContain("example.com");
    });

    it("waits for load state", async () => {
      await page.goto("https://example.com");
      await page.waitForLoadState();
      const title = await page.title();
      expect(title).toContain("Example Domain");
    });

    // ── Evaluation ────────────────────────────────────────────────────

    it("evaluates JS and returns value", async () => {
      const result = await page.evaluate("1 + 1");
      expect(result).toBe(2);
    });

    it("evaluates JS and returns string", async () => {
      const result = await page.evaluateStr("document.title");
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

    // ── Selectors ─────────────────────────────────────────────────────

    it("finds element text with selector", async () => {
      await page.goto("https://example.com");
      const text = await page.innerText("h1");
      expect(text).toBe("Example Domain");
    });

    it("gets innerHTML", async () => {
      const html = await page.innerHtml("h1");
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

    it("returns hidden for nonexistent selector", async () => {
      const hidden = await page.isHidden("#does-not-exist");
      expect(hidden).toBe(true);
    });

    it("returns not visible for nonexistent selector", async () => {
      const visible = await page.isVisible("#does-not-exist");
      expect(visible).toBe(false);
    });

    // ── Locator ───────────────────────────────────────────────────────

    it("creates a locator", () => {
      const loc = page.locator("h1");
      expect(loc.selector).toBe("h1");
    });

    it("gets text content via locator", async () => {
      const text = await page.locator("h1").textContent();
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

    it("chains locators with sub-selector", () => {
      const loc = page.locator("div").locator("h1");
      expect(loc.selector).toBe("div >> h1");
    });

    it("gets all text contents", async () => {
      const texts = await page.locator("p").allTextContents();
      expect(texts.length).toBeGreaterThan(0);
    });

    it("creates locator with getByText", () => {
      const loc = page.getByText("Example Domain");
      expect(loc.selector).toContain("text=");
    });

    it("creates locator with getByRole", () => {
      const loc = page.getByRole("link", { name: "More information" });
      expect(loc.selector).toContain("role=link");
    });

    it("creates first/last/nth locators", () => {
      const loc = page.locator("p");
      expect(loc.first().selector).toBe("p >> nth=0");
      expect(loc.last().selector).toBe("p >> nth=-1");
      expect(loc.nth(2).selector).toBe("p >> nth=2");
    });

    it("filters locators", () => {
      const loc = page.locator("p").filter({ hasText: "information" });
      expect(loc.selector).toContain("has-text=information");
    });

    // ── Screenshots ───────────────────────────────────────────────────

    it("takes a page screenshot", async () => {
      const buf = await page.screenshot();
      expect(buf.length).toBeGreaterThan(0);
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
      const buf = await page.screenshotElement("h1");
      expect(buf.length).toBeGreaterThan(0);
    });

    // ── Viewport and emulation ────────────────────────────────────────

    it("sets viewport size", async () => {
      await page.setViewportSize(800, 600);
      const width = await page.evaluate("window.innerWidth");
      expect(width).toBe(800);
    });

    it("sets viewport with full config", async () => {
      await page.setViewport({
        width: 375,
        height: 812,
        deviceScaleFactor: 3,
        isMobile: true,
      });
      const width = await page.evaluate("window.innerWidth");
      expect(width).toBe(375);
    });

    it("sets user agent", async () => {
      await page.setUserAgent("FerridriverTest/1.0");
      const ua = await page.evaluateStr("navigator.userAgent");
      expect(ua).toBe("FerridriverTest/1.0");
    });

    it("sets locale", async () => {
      await page.setLocale("de-DE");
      await page.goto("https://example.com");
      const lang = await page.evaluateStr("navigator.language");
      expect(lang).toBe("de-DE");
    });

    it("sets timezone", async () => {
      await page.setTimezone("America/New_York");
      await page.goto("https://example.com");
      const tz = await page.evaluateStr(
        "Intl.DateTimeFormat().resolvedOptions().timeZone"
      );
      expect(tz).toBe("America/New_York");
    });

    it("emulates dark color scheme", async () => {
      await page.emulateMedia(undefined, "dark");
      await page.goto("https://example.com");
      const isDark = await page.evaluate(
        "window.matchMedia('(prefers-color-scheme: dark)').matches"
      );
      expect(isDark).toBe(true);
    });

    it("emulates reduced motion", async () => {
      await page.emulateMedia(undefined, undefined, "reduce");
      await page.goto("https://example.com");
      const isReduced = await page.evaluate(
        "window.matchMedia('(prefers-reduced-motion: reduce)').matches"
      );
      expect(isReduced).toBe(true);
    });

    // ── Cookies ───────────────────────────────────────────────────────

    it("sets and gets a cookie", async () => {
      await page.goto("https://example.com");
      await page.setCookie({
        name: "test",
        value: "hello",
        domain: ".example.com",
        path: "/",
        secure: true,
        httpOnly: false,
      });
      const cookies = await page.cookies();
      const found = cookies.find((c) => c.name === "test");
      expect(found).toBeDefined();
      expect(found!.value).toBe("hello");
    });

    it("deletes a specific cookie by name and domain", async () => {
      await page.deleteCookie("test", ".example.com");
      const cookies = await page.cookies();
      const found = cookies.find((c) => c.name === "test");
      expect(found).toBeUndefined();
    });

    it("clears all cookies", async () => {
      await page.setCookie({
        name: "a",
        value: "1",
        domain: ".example.com",
        path: "/",
        secure: false,
        httpOnly: false,
      });
      await page.clearCookies();
      const cookies = await page.cookies();
      expect(cookies.length).toBe(0);
    });

    // ── setContent and forms ──────────────────────────────────────────

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
      await page.waitForSelector("#name");

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

    // ── Locator actions ───────────────────────────────────────────────

    it("clicks a button and verifies effect", async () => {
      await page.setContent(`
        <button id="btn" onclick="document.getElementById('result').textContent = 'clicked'">Click me</button>
        <div id="result"></div>
      `);
      await page.waitForSelector("#btn");
      await page.locator("#btn").click();
      const text = await page.locator("#result").innerText();
      expect(text).toBe("clicked");
    });

    it("fills an input via locator", async () => {
      await page.setContent('<input id="input" type="text" />');
      await page.waitForSelector("#input");
      const loc = page.locator("#input");
      await loc.fill("test value");
      const val = await loc.inputValue();
      expect(val).toBe("test value");
    });

    it("clears an input via locator", async () => {
      await page.setContent('<input id="input" type="text" value="hello" />');
      await page.waitForSelector("#input");
      const loc = page.locator("#input");
      await loc.clear();
      const val = await loc.inputValue();
      expect(val).toBe("");
    });

    it("focuses and blurs an element", async () => {
      await page.setContent('<input id="input" type="text" />');
      await page.waitForSelector("#input");
      const loc = page.locator("#input");
      await loc.focus();
      const focused = await page.evaluateStr(
        "document.activeElement?.id || ''"
      );
      expect(focused).toBe("input");

      await loc.blur();
      const blurred = await page.evaluateStr(
        "document.activeElement?.tagName || ''"
      );
      expect(blurred.toLowerCase()).toBe("body");
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
      await page.waitForSelector("#target");
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

    // ── Mouse operations ──────────────────────────────────────────────

    it("right-clicks an element", async () => {
      await page.setContent(`
        <div id="ctx" style="width:100px;height:100px;background:red"></div>
        <div id="result"></div>
        <script>
          document.getElementById('ctx').addEventListener('contextmenu', (e) => {
            e.preventDefault();
            document.getElementById('result').textContent = 'right-clicked';
          });
        </script>
      `);
      await page.waitForSelector("#ctx");
      const box_ = await page.locator("#ctx").boundingBox();
      await page.clickAtOpts(box_!.x + 50, box_!.y + 50, "right");
      const text = await page.locator("#result").innerText();
      expect(text).toBe("right-clicked");
    });

    it("double-clicks an element", async () => {
      await page.setContent(`
        <div id="dbl" style="width:100px;height:100px;background:blue"></div>
        <div id="result"></div>
        <script>
          document.getElementById('dbl').addEventListener('dblclick', () => {
            document.getElementById('result').textContent = 'double-clicked';
          });
        </script>
      `);
      await page.waitForSelector("#dbl");
      const box_ = await page.locator("#dbl").boundingBox();
      await page.clickAtOpts(box_!.x + 50, box_!.y + 50, "left", 2);
      const text = await page.locator("#result").innerText();
      expect(text).toBe("double-clicked");
    });

    it("moves mouse and triggers mousemove", async () => {
      await page.setContent(`
        <div id="hover" style="width:200px;height:200px;background:green"></div>
        <div id="result"></div>
        <script>
          document.getElementById('hover').addEventListener('mousemove', (e) => {
            document.getElementById('result').textContent = 'moved';
          });
        </script>
      `);
      await page.waitForSelector("#hover");
      const box_ = await page.locator("#hover").boundingBox();
      await page.moveMouse(box_!.x + 100, box_!.y + 100);
      const text = await page.locator("#result").innerText();
      expect(text).toBe("moved");
    });

    it("smooth mouse movement triggers multiple events", async () => {
      await page.setContent(`
        <div id="track" style="width:300px;height:50px;background:yellow"></div>
        <div id="count">0</div>
        <script>
          let c = 0;
          document.getElementById('track').addEventListener('mousemove', () => {
            c++;
            document.getElementById('count').textContent = String(c);
          });
        </script>
      `);
      await page.waitForSelector("#track");
      const box_ = await page.locator("#track").boundingBox();
      await page.moveMouseSmooth(box_!.x + 10, box_!.y + 25, box_!.x + 290, box_!.y + 25, 5);
      const count = parseInt(await page.locator("#count").innerText());
      expect(count).toBeGreaterThanOrEqual(3);
    });

    it("drag and drop fires mousedown and mouseup", async () => {
      await page.setContent(`
        <div id="area" style="width:400px;height:400px;background:#eee;position:relative">
          <div id="draggable" style="width:50px;height:50px;background:orange;position:absolute;left:10px;top:10px"></div>
        </div>
        <div id="result"></div>
        <script>
          const r = document.getElementById('result');
          document.addEventListener('mousedown', () => r.textContent += 'down,');
          document.addEventListener('mouseup', () => r.textContent += 'up,');
          document.addEventListener('mousemove', () => { if (!r.textContent.includes('move')) r.textContent += 'move,'; });
        </script>
      `);
      await page.waitForSelector("#draggable");
      await page.dragAndDrop(35, 35, 200, 200);
      const text = await page.locator("#result").innerText();
      expect(text).toContain("down");
      expect(text).toContain("up");
    });
  });
}
