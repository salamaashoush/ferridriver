import { expect, test } from "bun:test";
import { launchForBackend } from "./_helpers.js";

for (const backend of ["cdp-pipe", "cdp-raw", "bidi", "webkit"]) {
  test(`${backend}: a lazy context lists pages through the same browser`, async () => {
    const browser = await launchForBackend(backend);
    try {
      expect(typeof browser.version()).toBe("string");
      expect(browser.version()).not.toBe("Unknown");
      const context = await browser.newContext();
      try {
        expect(context.browser()?.version()).toBe(browser.version());
        expect(await context.pages()).toEqual([]);
        const page = await context.newPage();
        await page.setContent("<title>sashoush</title>");
        const pages = await context.pages();
        expect(pages.length).toBe(1);
        expect(await pages[0].title()).toBe("sashoush");
      } finally {
        await context.close();
      }
    } finally {
      await browser.close();
    }
  });

  // Playwright: `browserContext.pages(): Page[]` is synchronous.
  test(`${backend}: pages() answers at once, with the pages a caller already holds`, async () => {
    const browser = await launchForBackend(backend);
    try {
      const context = await browser.newContext();
      expect(Array.isArray(context.pages())).toBe(true);
      expect(context.pages().length).toBe(0);
      const first = await context.newPage();
      const second = await context.newPage();
      const [a, b] = context.pages();
      expect([a === first, b === second, context.pages().length]).toEqual([true, true, 2]);
      await first.close();
      expect(context.pages().length).toBe(1);
      expect(context.pages()[0] === second).toBe(true);
      expect((await context.discoverPages()).length).toBe(1);
      await context.close();
      expect(context.pages()).toEqual([]);
    } finally {
      await browser.close();
    }
  });
}
