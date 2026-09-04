// `page.developerTools()` and `page.executeDeveloperTool()` — the tools
// a page offers about itself.
//
// A page answers a `devtoolstooldiscovery` event with named callables
// carrying JSON schemas, so an agent can use the site's own vocabulary
// rather than its DOM. `chrome-devtools-mcp` spends two tools on this;
// here it is two page methods, and because it is a DOM event rather
// than a protocol it answers the same on all four backends.

import { test, describe, expect } from '@ferridriver/test';

// A page exposing one group of three tools: one that echoes its input,
// one that hands back values JSON cannot carry, and one that throws.
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
            return {
              element: document.getElementById('target'),
              cycle,
              fn: () => 1,
              instance: new Map([['a', 1]]),
              plain: { nested: true },
            };
          },
        },
        {
          name: 'refuses',
          description: 'Always throws',
          inputSchema: { type: 'object', properties: {} },
          execute: () => {
            throw new Error('nope, not today');
          },
        },
      ],
    });
  });
</script></body></html>`;

// A group whose tools do not all have an `execute`, which the discovery
// contract rejects outright rather than half-registering.
const MALFORMED = `<html><body><script>
  window.addEventListener('devtoolstooldiscovery', (event) => {
    event.respondWith({
      name: 'broken',
      tools: [{ name: 'no_execute', description: 'missing its callable', inputSchema: {} }],
    });
  });
</script></body></html>`;

// One that answers from a microtask rather than synchronously, which
// only arrives because the dispatch waits a macrotask for it.
const DEFERRED = `<html><body><script>
  window.addEventListener('devtoolstooldiscovery', (event) => {
    Promise.resolve().then(() => {
      event.respondWith({
        name: 'late',
        tools: [{
          name: 'ping',
          description: 'answers late',
          inputSchema: { type: 'object', properties: {} },
          execute: () => 'pong',
        }],
      });
    });
  });
</script></body></html>`;

describe('page developer tools', () => {
  test('a page that exposes none answers with none', async ({ page }) => {
    await page.setContent('<html><body>nothing here</body></html>');
    expect(await page.developerTools()).toEqual([]);

    let message = '';
    try {
      await page.executeDeveloperTool('anything');
    } catch (e) {
      message = String(e);
    }
    expect(message.includes('no developer tools')).toBe(true);
  });

  test('lists what the page announced, schema and all', async ({ page }) => {
    await page.setContent(EXPOSING);
    const groups = await page.developerTools();

    expect(groups.length).toBe(1);
    expect(groups[0].name).toBe('shop');
    expect(groups[0].description).toBe('the site itself');
    expect(groups[0].tools.map((t) => t.name)).toEqual(['add_to_cart', 'awkward', 'refuses']);

    const add = groups[0].tools[0];
    expect(add.description).toBe('Add a product to the cart');
    // The schema crosses whole: it is the page's contract with its
    // caller, and a reader that summarised it would be inventing one.
    expect(add.inputSchema).toEqual({
      type: 'object',
      properties: { sku: { type: 'string' }, quantity: { type: 'number' } },
      required: ['sku'],
    });

    // Asked twice, answered once: a second discovery must not report
    // the first one's groups again.
    expect((await page.developerTools()).length).toBe(1);
  });

  test('runs one, with the arguments it was given', async ({ page }) => {
    await page.setContent(EXPOSING);
    await page.developerTools();

    expect(await page.executeDeveloperTool('add_to_cart', { sku: 'A-1', quantity: 3 })).toEqual({
      added: 'A-1',
      quantity: 3,
    });
    // The tool's own default applies, so the parameters really reached it.
    expect(await page.executeDeveloperTool('add_to_cart', { sku: 'B-2' })).toEqual({
      added: 'B-2',
      quantity: 1,
    });

    let missing = '';
    try {
      await page.executeDeveloperTool('no_such_tool');
    } catch (e) {
      missing = String(e);
    }
    expect(missing.includes('no_such_tool')).toBe(true);

    let threw = '';
    try {
      await page.executeDeveloperTool('refuses');
    } catch (e) {
      threw = String(e);
    }
    expect(threw.includes('nope, not today')).toBe(true);
  });

  test('replaces what JSON cannot carry, and keeps the element reachable', async ({ page }) => {
    await page.setContent(EXPOSING);
    await page.developerTools();
    const result = (await page.executeDeveloperTool('awkward')) as Record<string, unknown>;

    expect(result.plain).toEqual({ nested: true });
    expect(result.fn).toBe('<Function object>');
    expect(result.instance).toBe('<Map instance>');
    expect((result.cycle as Record<string, unknown>).name).toBe('loop');
    expect((result.cycle as Record<string, unknown>).self).toBe('<Circular reference>');

    // The element is not described, it is parked: the id names a slot
    // the page still holds, so the caller can go back for it.
    expect(result.element).toEqual({ stashedId: 'stashed-0' });
    const stashedId = await page.evaluate("window.__dtmcp.stashedElements[0].id");
    expect(stashedId).toBe('target');
  });

  test('rejects a group whose tools are not callable', async ({ page }) => {
    await page.setContent(MALFORMED);
    expect(await page.developerTools()).toEqual([]);
  });

  test('waits a turn for a listener that answers late', async ({ page }) => {
    await page.setContent(DEFERRED);
    const groups = await page.developerTools();
    expect(groups.length).toBe(1);
    expect(await page.executeDeveloperTool('ping')).toBe('pong');
  });
});
