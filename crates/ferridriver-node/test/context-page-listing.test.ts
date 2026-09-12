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
}
