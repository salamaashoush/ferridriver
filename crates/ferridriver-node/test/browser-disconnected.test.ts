// browser.on('disconnected') through the binding: once, with the
// browser's contexts closed and isConnected() false.
import { describe, it, expect } from "bun:test";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

for (const backend of BACKENDS) {
  describe(`[${backend}] browser disconnected`, () => {
    it("fires once on close, and the contexts close", async () => {
      const browser = await launchForBackend(backend);
      const context = await browser.newContext();
      await context.newPage();
      const seen: string[] = [];
      browser.on("disconnected", (arg: any) => seen.push(`disconnected:${arg.isConnected()}`));
      context.on("close", () => seen.push("context close"));
      const disconnected = browser.waitForEvent("disconnected");
      await browser.close();
      await disconnected;
      await browser.close();
      for (let i = 0; i < 100 && seen.length < 2; i++) await Bun.sleep(20);
      expect(seen.sort()).toEqual(["context close", "disconnected:false"]);
      expect(browser.isConnected()).toBe(false);
    });
  });
}
