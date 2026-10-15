// The Frame members the binding adds over core: handles, navigation and
// waits, setContent and tag injection, run against a child frame served
// over HTTP so a navigation has a response.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page, type Frame } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

const outcome = (work: Promise<unknown>) => work.then(() => "resolved", (e: Error) => e.message);

for (const backend of BACKENDS) {
  describe(`[${backend}] Frame API`, () => {
    let browser: Browser;
    let page: Page;
    let server: ReturnType<typeof Bun.serve>;
    let origin: string;

    beforeAll(async () => {
      server = Bun.serve({
        port: 0,
        fetch(request) {
          const path = new URL(request.url).pathname;
          const html = (body: string) => new Response(body, { headers: { "content-type": "text/html" } });
          if (path === "/host") return html('<title>Host</title><iframe name="child" src="/child"></iframe>');
          if (path === "/child") return html('<title>Child</title><input id="i"><button id="b">go</button>');
          if (path === "/next") return html("<title>Next</title><p>next</p>");
          return new Response("missing", { status: 404 });
        },
      });
      origin = `http://127.0.0.1:${server.port}`;
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
      server.stop(true);
    });

    async function child(): Promise<Frame> {
      await page.goto(`${origin}/host`);
      const frame = page.frame("child")!;
      await frame.waitForLoadState("load", { timeout: 5000 });
      return frame;
    }

    it("$, $$ and frameElement resolve in the child frame", async () => {
      const frame = await child();
      expect(await (await frame.$("#b"))!.textContent()).toBe("go");
      expect(await page.mainFrame().$("#b")).toBeNull();
      expect((await frame.$$("input, button")).length).toBe(2);
      expect(await outcome(frame.$("input, button", { strict: true }))).toContain("strict mode violation");
      expect(await (await frame.frameElement()).getAttribute("name")).toBe("child");
      expect(await outcome(page.mainFrame().frameElement())).toContain("Frame has been detached.");
    });

    it("goto, waitForNavigation and waitForURL follow the child frame", async () => {
      const frame = await child();
      const response = await frame.goto(`${origin}/next`, { waitUntil: "domcontentloaded" });
      expect(response!.status()).toBe(200);
      expect(frame.url()).toBe(`${origin}/next`);
      expect(page.url()).toBe(`${origin}/host`);

      const [next] = await Promise.all([
        frame.waitForNavigation({ url: /\/child$/ }),
        frame.evaluate(`location.href = ${JSON.stringify(`${origin}/child`)}`),
      ]);
      expect(next!.url()).toBe(`${origin}/child`);
      await frame.waitForURL(/\/child$/);
      expect(await outcome(frame.waitForURL("**/never", { timeout: 300 }))).toContain("Timeout");

      const [top] = await Promise.all([
        page.waitForNavigation({ waitUntil: "commit" }),
        page.evaluate(`location.href = ${JSON.stringify(`${origin}/next`)}`),
      ]);
      expect(top!.status()).toBe(200);
    });

    it("setContent, addScriptTag and addStyleTag work in the child frame", async () => {
      const frame = await child();
      await frame.setContent("<title>Written</title><script>window.ran = 1</script>", { waitUntil: "load" });
      expect(await frame.evaluate("window.ran")).toBe(1);
      expect(await frame.title()).toBe("Written");
      expect(await page.title()).toBe("Host");

      const script = await frame.addScriptTag({ content: "window.tagged = 7" });
      expect(await script.evaluate((e: Element) => e.tagName)).toBe("SCRIPT");
      expect(await frame.evaluate("window.tagged")).toBe(7);
      const style = await frame.addStyleTag({ content: "body { margin-left: 9px }" });
      expect(await style.evaluate((e: Element) => e.tagName)).toBe("STYLE");
      expect(await frame.evaluate("getComputedStyle(document.body).marginLeft")).toBe("9px");
      expect(await outcome(frame.addScriptTag())).toContain("Provide an object with a `url`, `path` or `content` property");
    });

    it("waitForFunction and waitForTimeout run against the child frame", async () => {
      const frame = await child();
      const handle = await frame.waitForFunction("document.title === 'Child' && window.name");
      expect(await handle.jsonValue()).toBe("child");
      expect(await outcome(frame.waitForFunction("false", undefined, { timeout: 300 }))).toContain("Timeout");
      const started = Date.now();
      await frame.waitForTimeout(100);
      expect(Date.now() - started).toBeGreaterThanOrEqual(90);
    });
  });
}
