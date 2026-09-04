// NAPI coverage for page.developerTools() / page.executeDeveloperTool(),
// the tools a page offers about itself.
//
// A DOM event rather than a protocol, so the e2e specs run it on all
// four backends; this is the binding surface — the schema crosses whole
// rather than as `any`, the parameters reach the page, and what JSON
// cannot carry comes back replaced rather than as `{}`.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND ? [process.env.FERRIDRIVER_BACKEND] : ["cdp-pipe"];

const EXPOSING = `<html><body><main id="target">tools</main><script>
  window.addEventListener('devtoolstooldiscovery', (event) => {
    event.respondWith({
      name: 'shop',
      description: 'the site itself',
      tools: [
        {
          name: 'add_to_cart',
          description: 'Add a product to the cart',
          inputSchema: {
            type: 'object',
            properties: { sku: { type: 'string' }, quantity: { type: 'number' } },
            required: ['sku'],
          },
          execute: async (args) => ({ added: args.sku, quantity: args.quantity ?? 1 }),
        },
        {
          name: 'awkward',
          description: 'Returns what JSON cannot carry',
          inputSchema: { type: 'object', properties: {} },
          execute: () => {
            const cycle = { name: 'loop' };
            cycle.self = cycle;
            return { element: document.getElementById('target'), cycle, fn: () => 1, instance: new Map() };
          },
        },
      ],
    });
  });
</script></body></html>`;

for (const backend of BACKENDS) {
  describe(`page developer tools [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("answers with none where the page exposes none", async () => {
      await page.setContent("<html><body>nothing here</body></html>");
      expect(await page.developerTools()).toEqual([]);
      expect(page.executeDeveloperTool("anything")).rejects.toThrow(/no developer tools/);
    });

    it("lists the group, its tools, and each schema whole", async () => {
      await page.setContent(EXPOSING);
      const groups = await page.developerTools();

      expect(groups.length).toBe(1);
      expect(groups[0].name).toBe("shop");
      expect(groups[0].description).toBe("the site itself");
      expect(groups[0].tools.map((t) => t.name)).toEqual(["add_to_cart", "awkward"]);
      // The schema is the page's contract with its caller and crosses
      // unread, so a nested `required` array has to survive intact.
      expect(groups[0].tools[0].inputSchema).toEqual({
        type: "object",
        properties: { sku: { type: "string" }, quantity: { type: "number" } },
        required: ["sku"],
      });
      expect(groups[0].tools[0].annotations).toBeUndefined();
    });

    it("runs one with the arguments it was given", async () => {
      await page.setContent(EXPOSING);
      await page.developerTools();

      expect(await page.executeDeveloperTool("add_to_cart", { sku: "A-1", quantity: 3 })).toEqual({
        added: "A-1",
        quantity: 3,
      });
      // The tool's own default fills in, so the parameters really arrived.
      expect(await page.executeDeveloperTool("add_to_cart", { sku: "B-2" })).toEqual({
        added: "B-2",
        quantity: 1,
      });
      expect(page.executeDeveloperTool("no_such_tool")).rejects.toThrow(/no_such_tool/);
    });

    it("replaces what JSON cannot carry, and parks the element", async () => {
      await page.setContent(EXPOSING);
      await page.developerTools();
      const result = (await page.executeDeveloperTool("awkward")) as Record<string, unknown>;

      expect(result.fn).toBe("<Function object>");
      expect(result.instance).toBe("<Map instance>");
      expect((result.cycle as Record<string, unknown>).self).toBe("<Circular reference>");
      // Parked, not described: the id names a slot the page still holds.
      expect(result.element).toEqual({ stashedId: "stashed-0" });
      expect(await page.evaluate("window.__dtmcp.stashedElements[0].id")).toBe("target");
    });
  });
}
