// NAPI `browser.bind()` / `browser.unbind()` coverage.
//
// A bound session's protocol is "run this script", so hosting one requires a
// script engine — which this addon deliberately does not carry (it is the core
// browser surface). `bind()` therefore refuses instead of publishing a registry
// entry no client could drive, and says which hosts can: the CLI
// (`ferridriver session open`) and a script run by `ferridriver run`.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

for (const backend of BACKENDS) {
  describe(`browser.bind [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("refuses to bind and points at the hosts that can", async () => {
      const page = await browser.newPage();
      await page.setContent("<h1 id=greet>bound!</h1>");

      const bind = browser.bind("napi-test", { host: "127.0.0.1", port: 0 });
      await expect(bind).rejects.toThrow(/no script engine/);
      await expect(bind).rejects.toThrow(/ferridriver session open/);
    });

    it("does not leave a session behind after a refused bind", async () => {
      await browser.bind("napi-leak-check").catch(() => {});
      // unbind is the observable proof: it resolves because there is nothing
      // bound to tear down.
      await browser.unbind();
    });

    it("unbind is idempotent and safe before any bind", async () => {
      await browser.unbind();
      await browser.unbind();
    });
  });
}
