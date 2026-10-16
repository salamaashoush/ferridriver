// Members the Node binding lacked while core had them: browserType(),
// fileChooser.page(), webSocketRoute.protocols() and firefoxUserPrefs.
import { describe, it, expect } from "bun:test";
import { chromium, firefox } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

const NAMES: Record<string, string> = { "cdp-pipe": "chromium", "cdp-raw": "chromium", webkit: "webkit", bidi: "firefox" };

for (const backend of BACKENDS) {
  describe(`[${backend}] binding members`, () => {
    it("browserType, fileChooser.page and webSocketRoute.protocols", async () => {
      const browser = await launchForBackend(backend);
      try {
        expect(browser.browserType().name()).toBe(NAMES[backend]);
        const page = await browser.newPage();
        await page.setContent("<input type=file>");
        const [chooser] = await Promise.all([page.waitForEvent("filechooser"), page.click("input")]);
        expect((chooser as any).page()).toBe(page);

        let resolveAsked: (protocols: string[]) => void = () => {};
        const asked = new Promise<string[]>((resolve) => (resolveAsked = resolve));
        await page.routeWebSocket("ws://socket.example.com/", (route: any) => resolveAsked(route.protocols()));
        await page.evaluate("new WebSocket('ws://socket.example.com/', ['chat', 'superchat']); null");
        expect(await asked).toEqual(["chat", "superchat"]);
      } finally {
        await browser.close();
      }
    });
  });
}

describe("launch options", () => {
  it("firefoxUserPrefs reach Firefox", async () => {
    const browser = await firefox().launch({ firefoxUserPrefs: { "general.useragent.override": "acme-pref-probe" } });
    try {
      const page = await browser.newPage();
      expect(await page.evaluate("navigator.userAgent")).toBe("acme-pref-probe");
      expect(browser.browserType().name()).toBe("firefox");
    } finally {
      await browser.close();
    }
  }, 60_000);

  it("a launched browser answers with the type that launched it", async () => {
    const browser = await chromium({ transport: "ws" }).launch();
    try {
      expect(browser.browserType().name()).toBe("chromium");
    } finally {
      await browser.close();
    }
  });
});
