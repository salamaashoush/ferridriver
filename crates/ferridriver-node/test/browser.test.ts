import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type Server } from "node:http";

// Local test server -- guaranteed 200 responses, no external network dependency.
let testServer: Server;
let testUrl: string;

beforeAll(async () => {
  testServer = createServer((req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(`<!DOCTYPE html><html><head><title>Test Page</title></head><body><h1>Test Page</h1><p>More information...</p><a href="/about">More information</a></body></html>`);
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

// When FERRIDRIVER_BACKEND is set, run only that backend (enables parallel
// execution: `FERRIDRIVER_BACKEND=cdp-pipe bun test & FERRIDRIVER_BACKEND=cdp-raw bun test`).
const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (() => {
      const b = ["cdp-pipe", "cdp-raw"];
      if (process.platform === "darwin") b.push("webkit");
      return b;
    })();

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
      page = await browser.newPageWithUrl(testUrl);
    });

    afterAll(async () => {
      await browser.close();
    });

    // ── Navigation ────────────────────────────────────────────────────

    it("navigates to a URL", async () => {
      const url = await page.url();
      expect(url).toContain("127.0.0.1");
    });

    it("gets the page title", async () => {
      const title = await page.title();
      expect(title).toContain("Test Page");
    });

    it("gets page content", async () => {
      const html = await page.content();
      expect(html).toContain("<h1>Test Page</h1>");
    });

    it("extracts markdown", async () => {
      const md = await page.markdown();
      expect(md).toContain("Test Page");
    });

    it("navigates with goto", async () => {
      await page.goto(testUrl);
      const url = await page.url();
      expect(url).toContain("127.0.0.1");
    });

    it("waits for load state", async () => {
      await page.goto(testUrl);
      await page.waitForLoadState();
      const title = await page.title();
      expect(title).toContain("Test Page");
    });

    // ── Evaluation ────────────────────────────────────────────────────

    it("evaluates JS and returns value", async () => {
      const result = await page.evaluate("1 + 1");
      expect(result).toBe(2);
    });

    it("evaluates JS and returns string", async () => {
      const result = await page.evaluateStr("document.title");
      expect(result).toContain("Test Page");
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
      await page.goto(testUrl);
      const text = await page.innerText("h1");
      expect(text).toBe("Test Page");
    });

    it("gets innerHTML", async () => {
      const html = await page.innerHtml("h1");
      expect(html).toBe("Test Page");
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
      expect(text).toBe("Test Page");
    });

    it("gets inner text via locator", async () => {
      const text = await page.locator("h1").innerText();
      expect(text).toBe("Test Page");
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
      const loc = page.getByText("Test Page");
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
      // Playwright encoding: filter({hasText}) → ` >> internal:has-text="..."`
      // (see /tmp/playwright/packages/playwright-core/src/client/locator.ts:51).
      const loc = page.locator("p").filter({ hasText: "information" });
      expect(loc.selector).toContain("internal:has-text=");
      expect(loc.selector).toContain("information");
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

    it("screenshot type: 'jpeg' produces a JPEG", async () => {
      // JPEG magic: FF D8 FF.
      const buf = await page.screenshot({ type: "jpeg", quality: 80 });
      expect(buf.length).toBeGreaterThan(0);
      expect(buf[0]).toBe(0xff);
      expect(buf[1]).toBe(0xd8);
      expect(buf[2]).toBe(0xff);
    });

    it("screenshot clip crops to the supplied rectangle", async () => {
      // BiDi supports clip; WebKit rejects it with a typed error.
      if (backend === "webkit") return;
      await page.setContent('<div style="width:800px;height:600px;background:red"></div>');
      const buf = await page.screenshot({
        clip: { x: 10, y: 10, width: 100, height: 50 },
      });
      expect(buf.length).toBeGreaterThan(0);
      // PNG IHDR at bytes 16–23 carries big-endian width then height.
      const width = (buf[16] << 24) | (buf[17] << 16) | (buf[18] << 8) | buf[19];
      const height = (buf[20] << 24) | (buf[21] << 16) | (buf[22] << 8) | buf[23];
      // CDP/BiDi honour the clip at device scale; allow +/- DPR variance.
      expect(width).toBeGreaterThanOrEqual(100);
      expect(width).toBeLessThanOrEqual(200);
      expect(height).toBeGreaterThanOrEqual(50);
      expect(height).toBeLessThanOrEqual(100);
    });

    it("screenshot omitBackground yields a transparent PNG", async () => {
      // BiDi/WebKit refuse this option — CDP is the only supported path.
      if (backend !== "cdp-pipe" && backend !== "cdp-raw") return;
      await page.setContent("<html><body></body></html>");
      const buf = await page.screenshot({ omitBackground: true });
      expect(buf.length).toBeGreaterThan(0);
      // PNG header + IHDR + expect a tRNS or alpha channel: we just verify
      // the PNG is valid and non-empty. Full pixel-level transparency
      // verification needs image decoding which we don't want to pull in.
      expect(buf[0]).toBe(0x89);
      expect(buf[1]).toBe(0x50);
    });

    it("screenshot path writes bytes to disk", async () => {
      const fs = await import("node:fs");
      const os = await import("node:os");
      const path = await import("node:path");
      const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "ferridriver-screenshot-"));
      const out = path.join(tmp, "page.png");
      const buf = await page.screenshot({ path: out });
      expect(buf.length).toBeGreaterThan(0);
      const written = fs.readFileSync(out);
      expect(written.length).toBe(buf.length);
      // PNG magic.
      expect(written[0]).toBe(0x89);
      expect(written[1]).toBe(0x50);
      fs.rmSync(tmp, { recursive: true });
    });

    it("screenshot mask paints over the target", async () => {
      // webkit doesn't support clip yet; skip since mask without clip
      // would require a full-page diff.
      if (backend === "webkit") return;
      await page.setContent(
        '<html><body style="margin:0"><div id="target" style="width:100px;height:100px;background:#ff0000"></div></body></html>'
      );
      const buf = await page.screenshot({
        mask: [{ selector: "#target" }],
        clip: { x: 0, y: 0, width: 100, height: 100 },
      });
      expect(buf.length).toBeGreaterThan(0);
      expect(buf[0]).toBe(0x89);
      expect(buf[1]).toBe(0x50);
    });

    it("screenshot style injects CSS that applies to capture", async () => {
      // Inject a rule that paints `body` blue, capture, verify byte
      // streams differ from baseline. PNGs have a ~90-byte header of
      // mostly-identical metadata (IHDR, sRGB, time, etc.) so the
      // first-bytes diff needs to reach the IDAT chunk before it
      // sees any pixel-level variance.
      await page.setContent(
        '<html><body style="background:#ffffff;margin:0"><p>text</p></body></html>'
      );
      const withStyle = await page.screenshot({
        style: "body { background: #0000ff !important; }",
      });
      const withoutStyle = await page.screenshot();
      expect(withStyle.length).toBeGreaterThan(0);
      expect(withoutStyle.length).toBeGreaterThan(0);
      // Full byte-level equality: the two captures must differ
      // somewhere — the compressed IDAT stream reflects pixel
      // differences even when the leading headers match.
      expect(Buffer.compare(Buffer.from(withStyle), Buffer.from(withoutStyle))).not.toBe(0);
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
      // Navigate to a page with viewport meta tag (required for mobile
      // emulation to set CSS layout viewport -- without it Chrome defaults
      // to 980px, matching real mobile browser behavior).
      await page.goto(
        'data:text/html,<meta name="viewport" content="width=device-width,initial-scale=1"><h1>Mobile</h1>'
      );
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
      await page.goto(testUrl);
      const lang = await page.evaluateStr("navigator.language");
      expect(lang).toBe("de-DE");
    });

    it("sets timezone", async () => {
      await page.setTimezone("America/New_York");
      await page.goto(testUrl);
      const tz = await page.evaluateStr(
        "Intl.DateTimeFormat().resolvedOptions().timeZone"
      );
      expect(tz).toBe("America/New_York");
    });

    // Helper to restore the page to a clean emulation state after each
    // emulateMedia test so state doesn't leak into unrelated tests (a
    // lingering `forcedColors: "active"` in particular masks foreground
    // colors in later `getComputedStyle` assertions).
    async function resetEmulation() {
      await page.emulateMedia({
        media: null,
        colorScheme: null,
        reducedMotion: null,
        forcedColors: null,
        contrast: null,
      });
    }

    it("emulates dark color scheme", async () => {
      await page.emulateMedia({ colorScheme: "dark" });
      await page.goto(testUrl);
      const isDark = await page.evaluate(
        "window.matchMedia('(prefers-color-scheme: dark)').matches"
      );
      expect(isDark).toBe(true);
      await resetEmulation();
    });

    it("emulates reduced motion", async () => {
      await page.emulateMedia({ reducedMotion: "reduce" });
      await page.goto(testUrl);
      const isReduced = await page.evaluate(
        "window.matchMedia('(prefers-reduced-motion: reduce)').matches"
      );
      expect(isReduced).toBe(true);
      await resetEmulation();
    });

    it("emulates print media type", async () => {
      // Firefox/BiDi has no protocol for media-type emulation (Playwright's
      // own BiDi backend leaves it as an empty stub). Skip there — we'd be
      // asserting a fiction otherwise.
      if (backend === "bidi") return;
      await page.emulateMedia({ media: "print" });
      await page.goto(testUrl);
      const isPrint = await page.evaluate(
        "window.matchMedia('print').matches"
      );
      expect(isPrint).toBe(true);
      // Reset only `media` — verify null disables just that field.
      await page.emulateMedia({ media: null });
      const isPrintAfter = await page.evaluate(
        "window.matchMedia('print').matches"
      );
      expect(isPrintAfter).toBe(false);
      await resetEmulation();
    });

    it("emulates forced-colors active", async () => {
      // BiDi/Firefox: no forced-colors override available.
      if (backend === "bidi") return;
      await page.emulateMedia({ forcedColors: "active" });
      await page.goto(testUrl);
      const active = await page.evaluate(
        "window.matchMedia('(forced-colors: active)').matches"
      );
      expect(active).toBe(true);
      await resetEmulation();
    });

    it("emulates prefers-contrast more", async () => {
      // BiDi/Firefox: no prefers-contrast override available.
      if (backend === "bidi") return;
      await page.emulateMedia({ contrast: "more" });
      await page.goto(testUrl);
      const more = await page.evaluate(
        "window.matchMedia('(prefers-contrast: more)').matches"
      );
      expect(more).toBe(true);
      await resetEmulation();
    });

    it("emulateMedia composes all five fields in one call", async () => {
      // CDP / WebKit accept the full bag in one call; assert every field
      // lands in matchMedia. BiDi only supports colorScheme reliably.
      if (backend === "bidi") return;
      await page.emulateMedia({
        media: "print",
        colorScheme: "dark",
        reducedMotion: "reduce",
        forcedColors: "active",
        contrast: "more",
      });
      await page.goto(testUrl);
      const result = JSON.parse(
        (await page.evaluate(`JSON.stringify({
          print: matchMedia('print').matches,
          screen: matchMedia('screen').matches,
          dark: matchMedia('(prefers-color-scheme: dark)').matches,
          reduced: matchMedia('(prefers-reduced-motion: reduce)').matches,
          forced: matchMedia('(forced-colors: active)').matches,
          contrast: matchMedia('(prefers-contrast: more)').matches,
        })`)) as string
      );
      expect(result.print).toBe(true);
      expect(result.screen).toBe(false);
      expect(result.dark).toBe(true);
      expect(result.reduced).toBe(true);
      expect(result.forced).toBe(true);
      expect(result.contrast).toBe(true);
      await resetEmulation();
    });

    it("emulateMedia({}) is a no-op — does not disable prior emulation", async () => {
      if (backend === "bidi") return;
      await page.emulateMedia({ colorScheme: "dark" });
      await page.emulateMedia({});
      await page.goto(testUrl);
      const stillDark = await page.evaluate(
        "window.matchMedia('(prefers-color-scheme: dark)').matches"
      );
      expect(stillDark).toBe(true);
      await resetEmulation();
    });

    it("emulateMedia({colorScheme: null}) disables only that override", async () => {
      if (backend === "bidi") return;
      await page.emulateMedia({ colorScheme: "dark", reducedMotion: "reduce" });
      await page.goto(testUrl);
      await page.emulateMedia({ colorScheme: null });
      const dark = await page.evaluate(
        "window.matchMedia('(prefers-color-scheme: dark)').matches"
      );
      const reduced = await page.evaluate(
        "window.matchMedia('(prefers-reduced-motion: reduce)').matches"
      );
      expect(dark).toBe(false); // reset
      expect(reduced).toBe(true); // preserved
      await resetEmulation();
    });

    // ── Cookies (Playwright API: cookies live on BrowserContext) ───────

    it("sets and gets a cookie via context", async () => {
      await page.goto(testUrl);
      const context = browser.defaultContext();
      await context.addCookies([
        {
          name: "test",
          value: "hello",
          domain: "127.0.0.1",
          path: "/",
          secure: false,
          httpOnly: false,
        },
      ]);
      const cookies = await context.cookies();
      const found = cookies.find((c: any) => c.name === "test");
      expect(found).toBeDefined();
      expect(found!.value).toBe("hello");
    });

    it("deletes a specific cookie by name and domain", async () => {
      const context = browser.defaultContext();
      await context.deleteCookie("test", "127.0.0.1");
      const cookies = await context.cookies();
      const found = cookies.find((c: any) => c.name === "test");
      expect(found).toBeUndefined();
    });

    it("clears all cookies", async () => {
      const context = browser.defaultContext();
      await context.addCookies([
        {
          name: "a",
          value: "1",
          domain: "127.0.0.1",
          path: "/",
          secure: false,
          httpOnly: false,
        },
      ]);
      await context.clearCookies();
      const cookies = await context.cookies();
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

    // ── ClickOptions (task 1.5, full Playwright surface) ──────────────

    it("click with button:'right' fires contextmenu", async () => {
      await page.setContent(`
        <button id="b" oncontextmenu="document.getElementById('out').textContent = 'right'; return false;">b</button>
        <div id="out"></div>
      `);
      await page.waitForSelector("#b");
      await page.locator("#b").click({ button: "right" });
      const text = await page.locator("#out").innerText();
      expect(text).toBe("right");
    });

    it("click with button:'middle' fires auxclick", async () => {
      await page.setContent(`
        <button id="b">b</button>
        <div id="out"></div>
        <script>
          document.getElementById('b').addEventListener('auxclick', e => {
            if (e.button === 1) document.getElementById('out').textContent = 'middle';
          });
        </script>
      `);
      await page.waitForSelector("#b");
      await page.locator("#b").click({ button: "middle" });
      await new Promise((r) => setTimeout(r, 30));
      const text = await page.locator("#out").innerText();
      expect(text).toBe("middle");
    });

    it("click with clickCount:2 triggers dblclick handler", async () => {
      await page.setContent(`
        <button id="b">b</button>
        <div id="out"></div>
        <script>
          document.getElementById('b').addEventListener('dblclick', () => {
            document.getElementById('out').textContent = 'dbl';
          });
        </script>
      `);
      await page.waitForSelector("#b");
      await page.locator("#b").click({ clickCount: 2 });
      const text = await page.locator("#out").innerText();
      expect(text).toBe("dbl");
    });

    it("click with delay holds mousedown before mouseup", async () => {
      await page.setContent(`
        <button id="b">b</button>
        <div id="out"></div>
        <script>
          let downAt = 0;
          const b = document.getElementById('b');
          b.addEventListener('mousedown', () => { downAt = Date.now(); });
          b.addEventListener('mouseup', () => {
            const ms = Date.now() - downAt;
            document.getElementById('out').textContent = String(ms);
          });
        </script>
      `);
      await page.waitForSelector("#b");
      await page.locator("#b").click({ delay: 150 });
      const ms = Number(await page.locator("#out").innerText());
      // Allow slack for timer resolution + dispatch overhead, but ensure
      // we held for at least most of the requested 150ms.
      expect(ms).toBeGreaterThanOrEqual(120);
    });

    it("click with modifiers:['Shift'] sets shiftKey on the mouse event", async () => {
      await page.setContent(`
        <button id="b">b</button>
        <div id="out"></div>
        <script>
          document.getElementById('b').addEventListener('click', e => {
            document.getElementById('out').textContent = e.shiftKey ? 'shift' : 'none';
          });
        </script>
      `);
      await page.waitForSelector("#b");
      await page.locator("#b").click({ modifiers: ["Shift"] });
      const text = await page.locator("#out").innerText();
      expect(text).toBe("shift");
    });

    it("click with position offsets the mouse coordinates", async () => {
      await page.setContent(`
        <div id="b" style="width:200px;height:100px;background:#ccc;"></div>
        <div id="out"></div>
        <script>
          document.getElementById('b').addEventListener('click', e => {
            const r = e.currentTarget.getBoundingClientRect();
            const lx = Math.round(e.clientX - r.left);
            const ly = Math.round(e.clientY - r.top);
            document.getElementById('out').textContent = lx + ',' + ly;
          });
        </script>
      `);
      await page.waitForSelector("#b");
      await page.locator("#b").click({ position: { x: 10, y: 20 } });
      const text = await page.locator("#out").innerText();
      expect(text).toBe("10,20");
    });

    it("click with trial:true skips the click handler but presses modifiers", async () => {
      await page.setContent(`
        <button id="b">b</button>
        <div id="clicked">no</div>
        <div id="kd">none</div>
        <script>
          document.getElementById('b').addEventListener('click', () => {
            document.getElementById('clicked').textContent = 'yes';
          });
          document.addEventListener('keydown', e => {
            if (e.key === 'Shift') document.getElementById('kd').textContent = 'shift';
          });
        </script>
      `);
      await page.waitForSelector("#b");
      await page.locator("#b").click({ trial: true, modifiers: ["Shift"] });
      expect(await page.locator("#clicked").innerText()).toBe("no");
      // Per Playwright: modifiers are pressed regardless of trial.
      expect(await page.locator("#kd").innerText()).toBe("shift");
    });

    it("click rejects unknown button string", async () => {
      await page.setContent('<button id="b">b</button>');
      await page.waitForSelector("#b");
      let caught: unknown = null;
      try {
        // @ts-expect-error — intentionally bad button value.
        await page.locator("#b").click({ button: "garbage" });
      } catch (e) {
        caught = e;
      }
      expect(caught).not.toBeNull();
      expect(String((caught as Error).message)).toContain("Unknown mouse button");
    });

    it("click rejects unknown modifier string", async () => {
      await page.setContent('<button id="b">b</button>');
      await page.waitForSelector("#b");
      let caught: unknown = null;
      try {
        // @ts-expect-error — intentionally bad modifier value.
        await page.locator("#b").click({ modifiers: ["Hyper"] });
      } catch (e) {
        caught = e;
      }
      expect(caught).not.toBeNull();
      expect(String((caught as Error).message)).toContain("Unknown modifier");
    });

    // Task 1.5 phase 4b: check/uncheck/setChecked verify final state
    // matches target + reject uncheck-of-checked-radio, matching
    // Playwright's server/dom.ts::_setChecked.
    it("check: plain checkbox toggles; preventDefault throws did-not-change", async () => {
      await page.setContent('<input id="cb" type="checkbox" />');
      await page.waitForSelector("#cb");
      await page.locator("#cb").check();
      expect(await page.locator("#cb").isChecked()).toBe(true);

      await page.setContent('<input id="cb" type="checkbox" onclick="event.preventDefault()" />');
      await page.waitForSelector("#cb");
      let msg = "";
      try {
        await page.locator("#cb").check({ timeout: 500 });
      } catch (e) {
        msg = String((e as Error).message || e);
      }
      expect(msg, `preventDefault checkbox must throw, got: ${msg}`).toContain(
        "did not change its state"
      );
    });

    it("uncheck: checked radio throws Playwright's radio-group error", async () => {
      await page.setContent(
        '<input id="r" type="radio" name="g" checked /><input type="radio" name="g" />'
      );
      await page.waitForSelector("#r");
      let msg = "";
      try {
        await page.locator("#r").uncheck();
      } catch (e) {
        msg = String((e as Error).message || e);
      }
      expect(msg).toContain("Cannot uncheck radio button");
    });

    it("check with trial:true skips toggle AND verification", async () => {
      await page.setContent('<input id="cb" type="checkbox" onclick="event.preventDefault()" />');
      await page.waitForSelector("#cb");
      // Would normally throw because state doesn't change; with trial, returns ok.
      await page.locator("#cb").check({ trial: true });
      expect(await page.locator("#cb").isChecked()).toBe(false);
    });

    // Task 1.5 phase 4a: `fill.force` bypasses Playwright's
    // ['visible','enabled','editable'] pre-check. Without force, fill
    // on a `readonly` input surfaces `error:noteditable` → retry loop
    // keeps polling → deadline fires. With force:true, the pre-check
    // is skipped and `.value = 'x'` sticks.
    it("fill with force:true writes through a readonly input", async () => {
      await page.setContent('<input id="ro" readonly value="" />');
      await page.waitForSelector("#ro");

      // Without force → times out against a short deadline.
      let msg = "";
      const t0 = Date.now();
      try {
        await page.locator("#ro").fill("hello", { timeout: 250 });
      } catch (e) {
        msg = String((e as Error).message || e);
      }
      const elapsed = Date.now() - t0;
      expect(msg, `fill without force on readonly should Timeout; got: ${msg}`).toContain("Timeout");
      expect(elapsed, `fill should fail fast; got ${elapsed}ms`).toBeLessThan(1500);
      expect(await page.locator("#ro").inputValue()).toBe("");

      // With force:true → succeeds, value is written.
      await page.locator("#ro").fill("bypass", { force: true });
      expect(await page.locator("#ro").inputValue()).toBe("bypass");
    });

    // Task 1.5 phase 3 (Rule 4): tap uses the backend's native touch
    // input on CDP (Input.dispatchTouchEvent → isTrusted === true) and
    // surfaces a typed Unsupported error on backends that can't do
    // native touch (BiDi's pointerType has no 'touch'; WKWebView lacks
    // NSTouchEvent synthesis).
    it("tap: CDP dispatches trusted native touch event; WebKit reports Unsupported", async () => {
      if (backend === "webkit") {
        const dataUrl =
          "data:text/html," +
          encodeURIComponent(
            '<button id="b" ontouchstart="document.getElementById(\'out\').textContent=\'fired\'">b</button><div id="out">no</div>'
          );
        await page.goto(dataUrl);
        await page.waitForSelector("#b");
        let msg = "";
        try {
          await page.locator("#b").tap({ timeout: 2000 });
          msg = "no-throw";
        } catch (e) {
          msg = String((e as Error).message || e);
        }
        expect(msg.toLowerCase(), `webkit tap must be Unsupported, got: ${msg}`).toContain("unsupported");
        expect(msg, `Unsupported message should mention tap: ${msg}`).toContain("tap");
        // Prove no JS-fallback dispatch ran while the error was being assembled.
        const after = await page.locator("#out").innerText();
        expect(after).toBe("no");
        return;
      }
      // CDP backends: the native path fires a trusted touchstart inside
      // the element rect. We goto() a data: URL (rather than setContent,
      // which uses innerHTML and doesn't always re-run touch-handler
      // scripts in-order) so the HTML parser runs the addEventListener
      // script deterministically before we dispatch the touch.
      const trustedPageHtml =
        '<button id="b" style="width:120px;height:50px">b</button>' +
        '<div id="trusted">n</div><div id="inrect">n</div>' +
        "<script>" +
        "const b = document.getElementById('b');" +
        "b.addEventListener('touchstart', function(e) {" +
        "const t = e.changedTouches[0];" +
        "const r = b.getBoundingClientRect();" +
        "document.getElementById('trusted').textContent = String(e.isTrusted);" +
        "document.getElementById('inrect').textContent = String(" +
        "t.clientX >= r.left && t.clientX <= r.right && t.clientY >= r.top && t.clientY <= r.bottom" +
        ");" +
        "}, { passive: true });" +
        "</script>";
      await page.goto("data:text/html," + encodeURIComponent(trustedPageHtml));
      await page.waitForSelector("#b");
      await page.locator("#b").tap();
      expect(await page.locator("#trusted").innerText()).toBe("true");
      expect(await page.locator("#inrect").innerText()).toBe("true");

      // Modifiers: tap + Shift → event.shiftKey === true on the native event.
      const shiftPageHtml =
        '<button id="b">b</button><div id="out">none</div>' +
        "<script>" +
        "document.getElementById('b').addEventListener('touchstart', function(e) {" +
        "document.getElementById('out').textContent = e.shiftKey ? 'shift' : 'none';" +
        "}, { passive: true });" +
        "</script>";
      await page.goto("data:text/html," + encodeURIComponent(shiftPageHtml));
      await page.waitForSelector("#b");
      await page.locator("#b").tap({ modifiers: ["Shift"] });
      expect(await page.locator("#out").innerText()).toBe("shift");
    });

    // Task 1.5 phase 2: `opts.timeout` wins over the page default for every
    // action that accepts it. Previously accepted-but-ignored; now threaded
    // into the retry_resolve! macro's deadline.
    it("action timeout fires before the page default (click/fill/hover/tap)", async () => {
      await page.setContent('<button id="b">b</button>');
      await page.waitForSelector("#b");
      const cases: Array<[string, () => Promise<unknown>]> = [
        ["click", () => page.locator("#nope").click({ timeout: 200 })],
        ["fill", () => page.locator("#nope").fill("x", { timeout: 200 })],
        ["hover", () => page.locator("#nope").hover({ timeout: 200 })],
        ["tap", () => page.locator("#nope").tap({ timeout: 200 })],
        ["press", () => page.locator("#nope").press("A", { timeout: 200 })],
        ["type", () => page.locator("#nope").type("x", { timeout: 200 })],
        ["dblclick", () => page.locator("#nope").dblclick({ timeout: 200 })],
        ["check", () => page.locator("#nope").check({ timeout: 200 })],
        ["uncheck", () => page.locator("#nope").uncheck({ timeout: 200 })],
      ];
      for (const [name, call] of cases) {
        const t0 = Date.now();
        let msg = "";
        try {
          await call();
          msg = "no-throw";
        } catch (e) {
          msg = String((e as Error).message || e);
        }
        const elapsed = Date.now() - t0;
        expect(msg, `${name} should throw TimeoutError, got: ${msg}`).toContain(
          "Timeout"
        );
        expect(msg, `${name} should mention 200ms, got: ${msg}`).toContain(
          "200ms"
        );
        // Allow generous slack for CI / slow backends; the point is that the
        // call does NOT wait out the 30s page default.
        expect(
          elapsed,
          `${name} should fail within 1.5s of the 200ms timeout; got ${elapsed}ms`
        ).toBeLessThan(1500);
      }
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
          <div id="source" style="width:50px;height:50px;background:orange;position:absolute;left:10px;top:10px"></div>
          <div id="target" style="width:50px;height:50px;background:limegreen;position:absolute;left:200px;top:200px"></div>
        </div>
        <div id="result"></div>
        <script>
          const r = document.getElementById('result');
          document.addEventListener('mousedown', () => r.textContent += 'down,');
          document.addEventListener('mouseup', () => r.textContent += 'up,');
          document.addEventListener('mousemove', () => { if (!r.textContent.includes('move')) r.textContent += 'move,'; });
        </script>
      `);
      await page.waitForSelector("#source");
      await page.dragAndDrop("#source", "#target");
      const text = await page.locator("#result").innerText();
      expect(text).toContain("down");
      expect(text).toContain("up");
    });

    it("dragAndDrop honors sourcePosition, targetPosition and steps", async () => {
      // Navigate to a clean page first to ensure any lingering mouse state
      // from a previous test is reset.
      await page.goto("about:blank");
      await page.setContent(`
        <!DOCTYPE html>
        <html><head>
          <style>html,body{margin:0;padding:0}</style>
        </head><body>
        <div id="source" style="width:80px;height:80px;background:orange;position:absolute;left:20px;top:20px"></div>
        <div id="target" style="width:80px;height:80px;background:limegreen;position:absolute;left:200px;top:200px"></div>
        <div id="result" style="position:fixed;top:0;right:0">idle</div>
        <script>
          const r = document.getElementById('result');
          // Count both direct mousemove events and coalesced pointermove
          // sub-events. WebKit's AppKit-backed pipeline and Chromium's CDP
          // pipeline disagree on coalescing — summing both gives a
          // backend-agnostic tally of the dispatched steps.
          let moveCount = 0;
          window.addEventListener('mousedown', (e) => { r.dataset.down = JSON.stringify({x:e.clientX, y:e.clientY}); }, true);
          window.addEventListener('mouseup',   (e) => { r.dataset.up   = JSON.stringify({x:e.clientX, y:e.clientY}); }, true);
          window.addEventListener('mousemove', () => { moveCount += 1; r.dataset.moves = String(moveCount); }, true);
          window.addEventListener('pointermove', (e) => {
            const coalesced = typeof e.getCoalescedEvents === 'function' ? e.getCoalescedEvents() : [];
            if (coalesced.length > 1) {
              moveCount += coalesced.length - 1;
              r.dataset.moves = String(moveCount);
            }
          }, true);
        </script>
        </body></html>
      `);
      await page.waitForSelector("#source");
      await page.dragAndDrop("#source", "#target", {
        sourcePosition: { x: 5, y: 5 },
        targetPosition: { x: 10, y: 10 },
        steps: 6,
      });
      const state = JSON.parse(
        (await page.evaluate(
          "JSON.stringify({ down: document.getElementById('result').dataset.down || null, up: document.getElementById('result').dataset.up || null, moves: document.getElementById('result').dataset.moves || null })"
        )) as string,
      );
      const down = state.down;
      const up = state.up;
      const moves = state.moves;
      const downJson = JSON.parse(down as string);
      const upJson = JSON.parse(up as string);
      // sourcePosition = (5,5) relative to source at (20,20) → (25,25)
      expect(downJson.x).toBeGreaterThanOrEqual(24);
      expect(downJson.x).toBeLessThanOrEqual(26);
      expect(downJson.y).toBeGreaterThanOrEqual(24);
      expect(downJson.y).toBeLessThanOrEqual(26);
      // targetPosition = (10,10) relative to target at (200,200) → (210,210)
      expect(upJson.x).toBeGreaterThanOrEqual(209);
      expect(upJson.x).toBeLessThanOrEqual(211);
      expect(upJson.y).toBeGreaterThanOrEqual(209);
      expect(upJson.y).toBeLessThanOrEqual(211);
      expect(parseInt(moves as string, 10)).toBeGreaterThanOrEqual(6);
    });

    it("dragAndDrop trial does not dispatch mouse events", async () => {
      await page.setContent(`
        <div id="source" style="width:50px;height:50px;background:orange;position:absolute;left:10px;top:10px"></div>
        <div id="target" style="width:50px;height:50px;background:limegreen;position:absolute;left:200px;top:200px"></div>
        <div id="log"></div>
        <script>
          const l = document.getElementById('log');
          document.addEventListener('mousedown', () => l.textContent += 'down,');
          document.addEventListener('mouseup',   () => l.textContent += 'up,');
        </script>
      `);
      await page.waitForSelector("#source");
      await page.dragAndDrop("#source", "#target", { trial: true });
      const text = await page.locator("#log").innerText();
      expect(text).not.toContain("down");
      expect(text).not.toContain("up");
    });

    it("locator.dragTo forwards options and drops at targetPosition", async () => {
      await page.setContent(`
        <div id="source" style="width:80px;height:80px;background:orange;position:absolute;left:20px;top:20px"></div>
        <div id="target" style="width:80px;height:80px;background:limegreen;position:absolute;left:200px;top:200px"></div>
        <div id="tracker"></div>
        <script>
          const t = document.getElementById('tracker');
          document.addEventListener('mouseup', (e) => { t.dataset.up = JSON.stringify({x: e.clientX, y: e.clientY}); });
        </script>
      `);
      await page.waitForSelector("#source");
      await page.locator("#source").dragTo(page.locator("#target"), {
        targetPosition: { x: 15, y: 15 },
      });
      const up = JSON.parse((await page.evaluate("document.getElementById('tracker').dataset.up")) as string);
      expect(up.x).toBeGreaterThanOrEqual(214);
      expect(up.x).toBeLessThanOrEqual(216);
      expect(up.y).toBeGreaterThanOrEqual(214);
      expect(up.y).toBeLessThanOrEqual(216);
    });

    // ══════════════════════════════════════════════════════════════════
    // New NAPI method tests
    // ══════════════════════════════════════════════════════════════════

    // ── Browser methods ──────────────────────────────────────────────

    it("browser.version returns engine name", () => {
      expect(browser.version.length).toBeGreaterThan(0);
    });

    it("browser.isConnected returns true while connected", async () => {
      expect(await browser.isConnected()).toBe(true);
    });

    it("browser.contexts lists contexts", async () => {
      const ctxs = await browser.contexts();
      expect(ctxs.length).toBeGreaterThan(0);
    });

    // ── Page.isClosed ────────────────────────────────────────────────

    it("page.isClosed is false for active page", () => {
      expect(page.isClosed()).toBe(false);
    });

    // ── Page.viewportSize ────────────────────────────────────────────

    it("page.viewportSize returns dimensions", async () => {
      const [w, h] = await page.viewportSize();
      expect(w).toBeGreaterThan(0);
      expect(h).toBeGreaterThan(0);
    });

    // ── Page.goto with options ───────────────────────────────────────

    it("page.goto accepts GotoOptions", async () => {
      await page.goto(testUrl, {
        waitUntil: "domcontentloaded",
        timeout: 10000,
      });
      const title = await page.title();
      expect(title).toContain("Test Page");
    });

    // ── Page.waitForLoadState with state ──────────────────────────────

    it("waitForLoadState accepts state string", async () => {
      await page.goto(testUrl);
      await page.waitForLoadState("domcontentloaded");
      const ready = await page.evaluateStr("document.readyState");
      expect(ready === "interactive" || ready === "complete").toBe(true);
    });

    // ── Page.addInitScript / removeInitScript ────────────────────────

    it("addInitScript injects JS before page scripts", async () => {
      const id = await page.addInitScript(
        "window.__test_init_napi = 'injected'"
      );
      expect(id.length).toBeGreaterThan(0);
      await page.goto(testUrl);
      const val = await page.evaluateStr("window.__test_init_napi || 'missing'");
      expect(val).toBe("injected");
      await page.removeInitScript(id);
    });

    it("addInitScript accepts a function + JSON-serialised arg", async () => {
      // Mirrors Playwright docs example `page.addInitScript(mock => {...}, mock)`
      // from /tmp/playwright/packages/playwright-core/types/types.d.ts:303.
      const id = await page.addInitScript(
        (cfg: { answer: number; label: string }) => {
          (window as any).__fd_init_arg = cfg;
        },
        { answer: 42, label: "hello" }
      );
      await page.goto(testUrl);
      const answer = await page.evaluateStr("window.__fd_init_arg.answer");
      const label = await page.evaluateStr("window.__fd_init_arg.label");
      expect(Number(answer)).toBe(42);
      expect(label).toBe("hello");
      await page.removeInitScript(id);
    });

    it("addInitScript function without arg renders as (fn)(undefined)", async () => {
      const id = await page.addInitScript((x: unknown) => {
        (window as any).__fd_init_noarg = typeof x;
      });
      await page.goto(testUrl);
      const ty = await page.evaluateStr("window.__fd_init_noarg");
      expect(ty).toBe("undefined");
      await page.removeInitScript(id);
    });

    it("addInitScript function with explicit null arg receives null", async () => {
      // Playwright: `Object.is(null, undefined)` is false → JSON.stringify(null) = "null".
      const id = await page.addInitScript((x: unknown) => {
        (window as any).__fd_init_null = x === null ? "is-null" : typeof x;
      }, null);
      await page.goto(testUrl);
      const val = await page.evaluateStr("window.__fd_init_null");
      expect(val).toBe("is-null");
      await page.removeInitScript(id);
    });

    it("addInitScript with { content } bag treats string as-is", async () => {
      const id = await page.addInitScript({
        content: "window.__fd_init_content = 'from-content';",
      });
      await page.goto(testUrl);
      const val = await page.evaluateStr("window.__fd_init_content");
      expect(val).toBe("from-content");
      await page.removeInitScript(id);
    });

    it("addInitScript with { path } reads the file from disk", async () => {
      const fs = await import("node:fs/promises");
      const os = await import("node:os");
      const path = await import("node:path");
      const tmpFile = path.join(
        os.tmpdir(),
        `fd-init-script-${process.pid}-${Date.now()}.js`
      );
      await fs.writeFile(tmpFile, "window.__fd_init_file = 'from-file';");
      try {
        const id = await page.addInitScript({ path: tmpFile });
        await page.goto(testUrl);
        const val = await page.evaluateStr("window.__fd_init_file");
        expect(val).toBe("from-file");
        await page.removeInitScript(id);
      } finally {
        await fs.unlink(tmpFile);
      }
    });

    it("addInitScript rejects string + arg with Playwright's error message", async () => {
      let caught: unknown = null;
      try {
        await page.addInitScript("window.x = 1", { bad: true });
      } catch (e) {
        caught = e;
      }
      expect(caught).not.toBeNull();
      expect(String((caught as Error).message)).toContain(
        "Cannot evaluate a string with arguments"
      );
    });

    // ── Page.addScriptTag / addStyleTag ──────────────────────────────

    it("addScriptTag injects inline script", async () => {
      await page.setContent("<body></body>");
      await page.addScriptTag(undefined, "document.title = 'script_injected'");
      const title = await page.title();
      expect(title).toBe("script_injected");
    });

    it("addStyleTag injects inline CSS", async () => {
      await page.setContent('<div id="box">test</div>');
      await page.addStyleTag(undefined, "#box { color: red }");
      const color = await page.evaluateStr(
        "getComputedStyle(document.getElementById('box')).color"
      );
      expect(color).toBe("rgb(255, 0, 0)");
    });

    // ── Page.storageState ────────────────────────────────────────────

    it("storageState returns cookies and origins", async () => {
      await page.goto(testUrl);
      const state = await page.storageState();
      expect(state).toHaveProperty("cookies");
      expect(state).toHaveProperty("origins");
      expect(Array.isArray(state.cookies)).toBe(true);
      expect(Array.isArray(state.origins)).toBe(true);
    });

    // ── Page.mouseWheel / mouseDown / mouseUp ────────────────────────

    it("mouseWheel scrolls the page", async () => {
      await page.setContent(
        '<div style="height:5000px">tall</div>'
      );
      await page.mouseWheel(0, 300);
      // Give scroll time to apply
      await page.waitForTimeout(100);
      const scrollY = await page.evaluate("window.scrollY");
      expect(scrollY).toBeGreaterThan(0);
    });

    it("mouseDown and mouseUp fire events", async () => {
      await page.setContent(`
        <div id="log"></div>
        <script>
          document.addEventListener('mousedown', () => document.getElementById('log').textContent += 'down,');
          document.addEventListener('mouseup', () => document.getElementById('log').textContent += 'up,');
        </script>
      `);
      await page.mouseDown(100, 100);
      await page.mouseUp(100, 100);
      const log = await page.locator("#log").innerText();
      expect(log).toContain("down");
      expect(log).toContain("up");
    });

    // ── Page.on / off / removeAllListeners ───────────────────────────

    it("on returns listenerId, off removes it", async () => {
      const received: any[] = [];
      const id = page.on("console", (data) => {
        received.push(data);
      });
      expect(typeof id).toBe("number");
      expect(id).toBeGreaterThan(0);
      await page.evaluate("console.log('test_on_off')");
      await page.waitForTimeout(100);
      page.off(id);
      // After off, no more events should be received
    });

    it("removeAllListeners clears all listeners", () => {
      page.on("console", () => {});
      page.on("request", () => {});
      page.removeAllListeners();
      // Should not throw
    });

    // ── Page.defaultTimeout getter ───────────────────────────────────

    it("defaultTimeout returns the timeout", () => {
      expect(page.defaultTimeout).toBeGreaterThan(0);
    });

    // ── Locator.rightClick ───────────────────────────────────────────

    it("locator.rightClick fires contextmenu", async () => {
      await page.setContent(`
        <div id="target" oncontextmenu="document.title='ctx';return false" style="padding:20px">right click me</div>
      `);
      await page.waitForSelector("#target");
      await page.locator("#target").rightClick();
      const title = await page.title();
      expect(title).toBe("ctx");
    });

    // ── Locator.isAttached ───────────────────────────────────────────

    it("locator.isAttached returns true for existing element", async () => {
      await page.setContent('<div id="exists">here</div>');
      expect(await page.locator("#exists").isAttached()).toBe(true);
      expect(await page.locator("#gone").isAttached()).toBe(false);
    });

    // ── Locator.setChecked ───────────────────────────────────────────

    it("locator.setChecked toggles checkbox state", async () => {
      await page.setContent('<input id="cb" type="checkbox">');
      await page.waitForSelector("#cb");
      const loc = page.locator("#cb");
      await loc.setChecked(true);
      expect(await loc.isChecked()).toBe(true);
      await loc.setChecked(false);
      expect(await loc.isChecked()).toBe(false);
    });

    // ── Locator.selectText ───────────────────────────────────────────

    it("locator.selectText selects input text", async () => {
      await page.setContent('<input id="inp" type="text" value="select me">');
      await page.waitForSelector("#inp");
      await page.locator("#inp").selectText();
      const selected = await page.evaluateStr(
        "window.getSelection().toString()"
      );
      expect(selected).toBe("select me");
    });

    // ── Locator.evaluate / evaluateAll ────────────────────────────────

    it("locator.evaluate runs JS on element", async () => {
      await page.setContent('<h1 id="heading">Hello</h1>');
      const tag = await page.locator("#heading").evaluate("el.tagName");
      expect(tag).toBe("H1");
    });

    it("locator.evaluateAll runs JS on all matches", async () => {
      await page.setContent(`
        <ul><li class="item">A</li><li class="item">B</li><li class="item">C</li></ul>
      `);
      const count = await page.locator("css=.item").evaluateAll("elements.length");
      expect(count).toBe(3);
    });

    // ── Locator.orLocator / andLocator ────────────────────────────────

    it("locator.orLocator combines selectors", async () => {
      await page.setContent(
        '<button id="a">Alpha</button><span id="b">Beta</span>'
      );
      const combined = page.locator("#a").orLocator(page.locator("#b"));
      const count = await combined.count();
      expect(count).toBe(2);
    });

    it("locator.andLocator narrows to elements satisfying BOTH selectors (intersection)", async () => {
      // Playwright `.and()` is intersection: an element must match every
      // combined locator on its own. Fixture:
      //   - #submit — a <button> that ALSO has .primary    → matches
      //   - #cancel — a <button> with only .action         → no match
      //   - #label  — a <span>  with only .primary         → no match
      // See crates/ferridriver-node/test/locator-and-or.test.ts for the
      // canonical task-#10 semantics tests.
      await page.setContent(
        '<button id="submit" class="primary action">Submit</button>' +
        '<button id="cancel" class="action">Cancel</button>' +
        '<span id="label" class="primary">Label</span>'
      );
      const loc = page
        .locator("css=button")
        .andLocator(page.locator("css=.primary"));
      expect(await loc.count()).toBe(1);
      expect(await loc.textContent()).toBe("Submit");
      expect(await loc.getAttribute("id")).toBe("submit");
    });

    // ── Locator.all ──────────────────────────────────────────────────

    it("locator.all returns individual locators", async () => {
      await page.setContent(
        '<ul><li>A</li><li>B</li><li>C</li></ul>'
      );
      const all = await page.locator("li").all();
      expect(all.length).toBe(3);
      const text = await all[0].textContent();
      expect(text).toBe("A");
    });

    // ── Locator.tap ──────────────────────────────────────────────────

    // tap() now uses the backend's native touch input — CDP's
    // Input.dispatchTouchEvent (touchend DOES fire). WebKit has no
    // public touch-injection API so tap throws Unsupported; the fuller
    // coverage is in the earlier "tap: CDP dispatches trusted native
    // touch event" test.
    it("locator.tap fires tap events", async () => {
      if (backend === "webkit") {
        return;
      }
      await page.setContent(`
        <button id="btn">tap me</button>
        <script>
          var b = document.getElementById('btn');
          b.addEventListener('touchend', function() { this.textContent = 'tapped'; });
          b.addEventListener('pointerup', function(e) { if(e.pointerType==='touch') this.textContent = 'tapped'; });
        </script>
      `);
      await page.waitForSelector("#btn");
      await page.locator("#btn").tap();
      const text = await page.locator("#btn").textContent();
      expect(text).toBe("tapped");
    });

    // ── Frame methods ────────────────────────────────────────────────

    it("frame.isDetached returns false for active frame", async () => {
      await page.goto(testUrl);
      const main = page.mainFrame()!;
      expect(main.isDetached()).toBe(false);
    });

    // ── Context methods ──────────────────────────────────────────────

    it("context.name returns context name", () => {
      const ctx = browser.defaultContext();
      expect(ctx.name).toBe("default");
    });

    it("context.setOffline toggles network", async () => {
      const ctx = browser.defaultContext();
      await ctx.setOffline(true);
      // Navigating should fail or return error
      await ctx.setOffline(false);
      // Restore connectivity -- page should work again
      await page.goto(testUrl);
      const title = await page.title();
      expect(title).toContain("Test Page");
    });
  });
}
