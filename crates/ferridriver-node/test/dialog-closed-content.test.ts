/**
 * page.on('dialogclosed') hands over the Dialog page.on('dialog') did,
 * and content({ includeShadow }) serializes open shadow roots.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] dialogclosed and content`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("fires dialogclosed with the opened dialog", async () => {
    let opened: unknown;
    const closed = new Promise<unknown>((resolve) => page.once("dialogclosed", resolve));
    page.once("dialog", (dialog: any) => {
      opened = dialog;
      void dialog.accept();
    });
    await page.evaluate("alert('yo')");
    expect((await closed) === opened).toBe(true);
  });

  it("includes open shadow roots when asked", async () => {
    await page.setContent('<div id="host"><template shadowrootmode="open"><b>shadow</b></template><i>light</i></div>');
    expect(await page.content()).toBe('<html><head></head><body><div id="host"><i>light</i></div></body></html>');
    expect(await page.content({ includeShadow: true })).toBe(
      '<html><head></head><body><div id="host"><template shadowrootmode="open"><b>shadow</b></template><i>light</i></div></body></html>',
    );
  });
});
