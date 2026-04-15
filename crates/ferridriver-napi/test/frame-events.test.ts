/**
 * Tests for Frame API and Event system.
 * Verifies Playwright-compatible behavior across all backends.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { Browser, type Page } from "../index.js";
import { createServer, type Server } from "node:http";

// Local test server -- guaranteed 200 responses, no external network dependency.
let testServer: Server;
let testUrl: string;

beforeAll(async () => {
  testServer = createServer((req, res) => {
    if (req.url === "/json") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ status: "ok" }));
    } else {
      res.writeHead(200, { "Content-Type": "text/html" });
      res.end(`<!DOCTYPE html><html><head><title>Test Page</title></head><body><h1>Hello</h1></body></html>`);
    }
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

// When FERRIDRIVER_BACKEND is set, run only that backend for parallel execution.
const CDP_BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND] as const
  : ["cdp-pipe", "cdp-raw"] as const;

for (const backend of CDP_BACKENDS) {
  describe(`[${backend}] Frames`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser?.close();
    });

    it("gets the main frame", async () => {
      await page.goto(testUrl);
      const main = await page.mainFrame();
      expect(main).toBeDefined();
      expect(main.isMainFrame()).toBe(true);
      expect(main.url).toContain("127.0.0.1");
    });

    it("main frame has no parent", async () => {
      const main = await page.mainFrame();
      const parent = await main.parentFrame();
      expect(parent).toBeNull();
    });

    it("gets all frames (no iframes = 1 frame)", async () => {
      await page.goto(testUrl);
      const frames = await page.frames();
      expect(frames.length).toBe(1);
      expect(frames[0].isMainFrame()).toBe(true);
    });

    it("detects iframes in frame tree", async () => {
      await page.setContent(`
        <h1>Parent</h1>
        <iframe name="child" srcdoc="<h1>Child</h1>"></iframe>
      `);
      // Wait for iframe to load
      await page.waitForTimeout(500);
      const frames = await page.frames();
      expect(frames.length).toBeGreaterThanOrEqual(2);
    });

    it("finds frame by name", async () => {
      await page.setContent(`
        <iframe name="myframe" srcdoc="<h1>Named Frame</h1>"></iframe>
      `);
      await page.waitForTimeout(500);
      const frame = await page.frame("myframe");
      expect(frame).not.toBeNull();
      expect(frame!.name).toBe("myframe");
    });

    it("evaluates JS in main frame", async () => {
      await page.setContent("<h1>Main</h1>");
      const main = await page.mainFrame();
      const title = await main.evaluateStr("document.querySelector('h1').textContent");
      expect(title).toBe("Main");
    });

    it("evaluates JS in child iframe", async () => {
      await page.setContent(`
        <h1>Parent</h1>
        <iframe name="child" srcdoc="<h1>Child Content</h1>"></iframe>
      `);
      await page.waitForTimeout(500);
      const frame = await page.frame("child");
      if (frame) {
        const text = await frame.evaluateStr("document.querySelector('h1')?.textContent || 'none'");
        expect(text).toBe("Child Content");
      }
    });

    it("creates frame-scoped locator", async () => {
      await page.setContent(`
        <h1>Parent Title</h1>
        <iframe name="child" srcdoc="<h1>Child Title</h1>"></iframe>
      `);
      await page.waitForTimeout(500);
      const frame = await page.frame("child");
      if (frame) {
        const loc = frame.locator("h1");
        expect(loc.selector).toBe("h1");
      }
    });

    it("main frame has child frames", async () => {
      await page.setContent(`
        <iframe name="a" srcdoc="<p>A</p>"></iframe>
        <iframe name="b" srcdoc="<p>B</p>"></iframe>
      `);
      await page.waitForTimeout(500);
      const main = await page.mainFrame();
      const children = await main.childFrames();
      expect(children.length).toBeGreaterThanOrEqual(2);
    });

    it("frame content() returns HTML", async () => {
      await page.goto(testUrl);
      const main = await page.mainFrame();
      const html = await main.content();
      expect(html).toContain("<h1>");
    });

    it("frame title() returns document title", async () => {
      await page.goto(testUrl);
      const main = await page.mainFrame();
      const title = await main.title();
      expect(title).toBe("Test Page");
    });
  });

  describe(`[${backend}] Events`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await Browser.launch({ backend });
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser?.close();
    });

    it("waitForResponse catches network response", async () => {
      const [response] = await Promise.all([
        page.waitForResponse("127.0.0.1", 10000),
        page.goto(testUrl),
      ]);
      expect(response).toBeDefined();
      expect(response.status).toBe(200);
      expect(response.url).toContain("127.0.0.1");
    });

    it("waitForResponse with navigation", async () => {
      const [response] = await Promise.all([
        page.waitForResponse("127.0.0.1", 10000),
        page.goto(testUrl),
      ]);
      expect(response.url).toContain("127.0.0.1");
      expect(response.status).toBe(200);
    });
  });
}


// ── Event callback tests ─────────────────────────────────────────────────

const EVENT_BACKENDS = ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])] as const;

for (const backend of EVENT_BACKENDS) {
describe(`Events - on/once/waitForEvent (${backend})`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await Browser.launch({ backend });
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser?.close();
  });

  it("page.on('console') receives console.log messages", async () => {
    const messages: any[] = [];
    page.on("console", (msg) => {
      messages.push(msg);
    });

    await page.setContent('<script>console.log("hello from page")</script>');
    await page.waitForTimeout(500);

    expect(messages.length).toBeGreaterThan(0);
    const found = messages.find((m: any) => m.text?.includes("hello from page"));
    expect(found).toBeDefined();
    expect(found.type).toBe("log");
  });

  it("page.once('console') fires only once", async () => {
    const messages: any[] = [];
    page.once("console", (msg) => {
      messages.push(msg);
    });

    await page.evaluate("console.log('first'); console.log('second')");
    // One event loop tick for the TSFN callback to fire
    await new Promise(r => setTimeout(r, 0));

    expect(messages.length).toBe(1);
  });

  // Response events only available on CDP backends (webkit doesn't track HTTP responses natively)
  if (backend !== "webkit") {
    it("page.waitForEvent('response') resolves on network response", async () => {
      const [event] = await Promise.all([
        page.waitForEvent("response", 10000),
        page.goto(testUrl),
      ]);
      expect(event).toBeDefined();
      expect((event as any).status).toBe(200);
      expect((event as any).url).toContain("127.0.0.1");
    });

    it("page.waitForResponse matches URL pattern", async () => {
      const [response] = await Promise.all([
        page.waitForResponse("127.0.0.1", 10000),
        page.goto(testUrl),
      ]);
      expect(response.status).toBe(200);
      expect(response.url).toContain("127.0.0.1");
    });

    it("page.on('response') fires for every request", async () => {
      const responses: any[] = [];
      page.on("response", (r) => {
        responses.push(r);
      });

      await page.goto(testUrl);
      await page.waitForTimeout(500);

      expect(responses.length).toBeGreaterThan(0);
      expect(responses[0].status).toBe(200);
      expect(responses[0].url).toContain("127.0.0.1");
    });
  }
});
} // end for loop
