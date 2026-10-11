// Ported from crates/ferridriver-cli/tests/backends_support/binding_surface.rs —
// every getBy* accessor on Frame and Locator, the FrameLocator class,
// page.touchscreen / page.snapshotForAI / page.exposeFunction /
// page.frameLocator, context-level expose, and
// context.clearCookies({...}) filters. Test titles mirror the original
// Rust fn names.

import { test, describe, expect } from '@ferridriver/test';
import type { BindingSource, Page } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

// A labelled button, an image with alt text, and an iframe so every
// getBy* path resolves.
const FIXTURE = dataUrl(
  "<button title='hi' aria-label='click-me'>x</button>" +
    "<img alt='kitten' src='data:image/gif;base64,R0lGODlhAQABAAAAACw='>" +
    "<iframe srcdoc='<button id=inner>inside</button>'></iframe>",
);

async function setup(page: Page): Promise<void> {
  await page.goto(FIXTURE);
  await page.waitForSelector("button[title='hi']");
}

describe('binding surface', () => {
  test('frame_get_by_methods', async ({ page }) => {
    await setup(page);
    const f = page.mainFrame();
    expect(await f.getByTitle('hi').textContent()).toBe('x');
    expect(await f.getByLabel('click-me').textContent()).toBe('x');
    expect(await f.getByAltText('kitten').isVisible()).toBe(true);
    expect(await f.getByRole('button').textContent()).toContain('x');
    expect(await f.getByText('x').textContent()).toBe('x');
    expect(typeof f.getByPlaceholder('z').click).toBe('function');
    expect(typeof f.getByTestId('z').click).toBe('function');
  });

  test('frame_page_and_frame_locator', async ({ page }) => {
    await setup(page);
    const f = page.mainFrame();
    const p = f.page();
    expect(p.url().startsWith('data:')).toBe(true);
    expect(typeof p.goto).toBe('function');
    expect(typeof f.frameLocator('iframe').locator).toBe('function');
  });

  test('locator_get_by_methods', async ({ page }) => {
    await setup(page);
    const body = page.locator('body');
    expect(typeof body.getByRole('button').click).toBe('function');
    expect(typeof body.getByText('x').click).toBe('function');
    expect(typeof body.getByTestId('z').click).toBe('function');
    expect(typeof body.getByLabel('click-me').click).toBe('function');
    expect(typeof body.getByPlaceholder('z').click).toBe('function');
    expect(typeof body.getByAltText('kitten').click).toBe('function');
    expect(typeof body.getByTitle('hi').click).toBe('function');
  });

  test('locator_page_and_frame_methods', async ({ page }) => {
    await setup(page);
    const loc = page.locator('iframe');
    expect(typeof loc.page().goto).toBe('function');
    expect(typeof loc.frameLocator('button').locator).toBe('function');
    expect(typeof loc.contentFrame().locator).toBe('function');
  });

  test('frame_locator_class', async ({ page }) => {
    await setup(page);
    const fl = page.frameLocator('iframe');
    expect(typeof fl.locator('body').click).toBe('function');
    expect(typeof fl.getByRole('button').click).toBe('function');
    expect(typeof fl.getByText('inside').click).toBe('function');
    expect(typeof fl.getByTestId('x').click).toBe('function');
    expect(typeof fl.getByLabel('x').click).toBe('function');
    expect(typeof fl.getByPlaceholder('x').click).toBe('function');
    expect(typeof fl.getByAltText('x').click).toBe('function');
    expect(typeof fl.getByTitle('x').click).toBe('function');
    expect(typeof fl.owner().click).toBe('function');
    expect(typeof fl.first().locator).toBe('function');
    expect(typeof fl.last().locator).toBe('function');
    expect(typeof fl.nth(0).locator).toBe('function');
    expect(typeof fl.frameLocator('iframe').locator).toBe('function');
  });

  test('page_touchscreen_tap', async ({ page }) => {
    // Native touch on every backend: CDP Input.dispatchTouchEvent,
    // WebKit Input.dispatchTapEvent, BiDi input.performActions with a
    // touch pointer source. The tap must produce a trusted touch
    // sequence the page can observe — not merely a call that doesn't
    // throw.
    await page.goto(
      dataUrl(
        '<div id=pad style="position:fixed;inset:0"></div><div id=out>none</div>' +
          '<script>' +
          "const pad = document.getElementById('pad');" +
          "pad.addEventListener('touchstart', e => { document.getElementById('out').textContent = 'touch:' + e.isTrusted; }, { passive: true });" +
          "pad.addEventListener('pointerdown', e => { if (e.pointerType === 'touch') document.getElementById('out').textContent = 'touch:' + e.isTrusted; });" +
          '</script>',
      ),
    );
    await page.waitForSelector('#pad');
    await page.touchscreen.tap(10, 10);
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('touch:true');
  });

  test('page_expose_function', async ({ page }) => {
    // Playwright parity: args are SPREAD into the callback and the
    // callback's return value is delivered to the page-side caller, so
    // `await window.fn(...)` resolves to the real result (no polling).
    await setup(page);
    await page.exposeFunction('__expose_record', (...a: unknown[]) => ({ got: a, n: a.length }));
    expect(await page.evaluate('typeof window.__expose_record')).toBe('function');
    expect(await page.evaluate("window.__expose_record(1, 'two', { three: 3 })")).toEqual({
      got: [1, 'two', { three: 3 }],
      n: 3,
    });
  });

  test('page_expose_function_errors', async ({ page }) => {
    // Ported from page-expose-function.spec.ts "should throw exception in
    // page context": the page-side call rejects with the callback's error,
    // whether it throws or returns a rejected promise.
    await setup(page);
    await page.exposeFunction('woof', () => {
      throw new Error('WOOF WOOF');
    });
    await page.exposeFunction('woofAsync', async () => {
      throw new Error('async WOOF');
    });
    const caught = (name: string) =>
      page.evaluate(`window.${name}().then(() => 'no error', (e) => e.message)`);
    expect(await caught('woof')).toBe('WOOF WOOF');
    expect(await caught('woofAsync')).toBe('async WOOF');
  });

  test('page_expose_binding', async ({ page }) => {
    // page.exposeBinding = page.exposeFunction plus a leading
    // BindingSource: the live { context, page, frame } the call came
    // from (types/structs.d.ts). Prove the source is those objects, the
    // spread args follow it, and the callback's return value reaches the
    // page-side caller.
    await setup(page);
    let seen: { keys: string[]; page: boolean; frame: boolean; pages: number } | null = null;
    await page.exposeBinding('__page_bind', async (source: BindingSource, ...a: number[]) => {
      seen = {
        keys: Object.keys(source).sort(),
        page: source.page === page,
        frame: source.frame === page.mainFrame(),
        pages: (await source.context.pages()).length,
      };
      return { sum: a.reduce((x, y) => x + y, 0), title: await source.page.title() };
    });
    expect(await page.evaluate('typeof window.__page_bind')).toBe('function');
    await page.evaluate("document.title = 'bound'");
    expect(await page.evaluate('window.__page_bind(2, 3, 5)')).toEqual({ sum: 10, title: 'bound' });
    expect(seen).toEqual({ keys: ['context', 'frame', 'page'], page: true, frame: true, pages: 1 });
  });

  test('a binding called from a frame is given that frame', async ({ page }) => {
    // Ported from browsercontext-expose-function.spec.ts "should work":
    // the callback acts through the frame it is handed, and what it
    // returns is a handle made in that frame.
    await setup(page);
    const inner = page.frames().find((f) => f !== page.mainFrame())!;
    await inner.waitForSelector('#inner');
    const frames: boolean[] = [];
    await page.exposeBinding('__add_handle', async (source: BindingSource, a: number, b: number) => {
      frames.push(source.frame === inner);
      return source.frame.evaluateHandle(([x, y]: number[]) => x + y, [a, b]);
    });
    expect(await page.evaluate('window.__add_handle(5, 6)')).toBe(11);
    expect(await inner.evaluate('window.__add_handle(20, 22)')).toBe(42);
    expect(frames).toEqual([false, true]);
  });

  test('context_expose_binding', async ({ browser }) => {
    // Register the binding BEFORE opening the page, then open a fresh
    // page in the context and observe that window[name] is present and
    // that the BindingSource object reached the callback — proving the
    // binding applied to a page created AFTER registration.
    const ctx = await browser.newContext();
    try {
      let opened: Page | null = null;
      let sourceIsPage: boolean | null = null;
      const disp = await ctx.exposeBinding('__ctx_bind', async (source: BindingSource, ...a: number[]) => {
        sourceIsPage = source.page === opened;
        return { sum: a.reduce((x, y) => x + y, 0), url: (await source.context.pages())[0].url() };
      });
      const p = await ctx.newPage();
      opened = p;
      const url = dataUrl('<title>x</title>');
      await p.goto(url);
      expect(await p.evaluate('typeof window.__ctx_bind')).toBe('function');
      expect(await p.evaluate('window.__ctx_bind(2, 3, 5)')).toEqual({ sum: 10, url });
      expect(sourceIsPage).toBe(true);
      // After dispose the page-side proxy is gone.
      await disp.dispose();
      expect(await p.evaluate('typeof window.__ctx_bind')).toBe('undefined');
    } finally {
      await ctx.close();
    }
  });

  test('context_expose_function', async ({ browser }) => {
    // exposeFunction = exposeBinding minus the source arg: the callback
    // sees ONLY the spread page-side args.
    const ctx = await browser.newContext();
    try {
      await ctx.exposeFunction('__ctx_fn', (...a: unknown[]) => ({ got: a, n: a.length }));
      const p = await ctx.newPage();
      await p.goto(dataUrl('<title>x</title>'));
      expect(await p.evaluate("window.__ctx_fn(1, 'two')")).toEqual({ got: [1, 'two'], n: 2 });
    } finally {
      await ctx.close();
    }
  });

  test('context function reaches a live child frame and leaves it on dispose', async ({ browser }) => {
    // Playwright evaluates a new binding's script in every frame
    // (`safeNonStallingEvaluateInAllFrames`), and its cleanup script too.
    const ctx = await browser.newContext();
    try {
      const p = await ctx.newPage();
      await setup(p);
      const frame = p.frames().find((f) => f !== p.mainFrame())!;
      await frame.waitForSelector('#inner');
      const disp = await ctx.exposeFunction('__late_fn', (a: number, b: number) => a + b);
      expect(await frame.evaluate('window.__late_fn(2, 3)')).toBe(5);
      await disp.dispose();
      expect(await frame.evaluate('typeof window.__late_fn')).toBe('undefined');
      expect(await p.evaluate('typeof window.__late_fn')).toBe('undefined');
    } finally {
      await ctx.close();
    }
  });

  test('context_clear_cookies_filter', async ({ browser }) => {
    // Playwright: clearCookies({ name }) removes only the matching
    // cookie. String filters are exact matches; RegExp filters .test()
    // the field. Strict on every backend — WebKit enumerates the
    // context store via Playwright.getAllCookies and BiDi partitions on
    // the user context's storage key.
    const ctx = await browser.newContext();
    try {
      const p = await ctx.newPage();
      await p.goto(dataUrl('<title>x</title>'));
      await ctx.addCookies([
        { name: 'keep', value: '1', domain: '.example.test', path: '/', secure: false, httpOnly: false, expires: -1 },
        { name: 'drop', value: '1', domain: '.example.test', path: '/', secure: false, httpOnly: false, expires: -1 },
        { name: 'drop2', value: '1', domain: '.example.test', path: '/', secure: false, httpOnly: false, expires: -1 },
      ]);
      const before = (await ctx.cookies()).map((c) => c.name).sort();
      expect(before).toEqual(['drop', 'drop2', 'keep']);
      await ctx.clearCookies({ name: 'drop' });
      const afterExact = (await ctx.cookies()).map((c) => c.name).sort();
      expect(afterExact).toEqual(['drop2', 'keep']);
      await ctx.clearCookies({ name: /^DROP/i });
      const afterRegex = (await ctx.cookies()).map((c) => c.name).sort();
      expect(afterRegex).toEqual(['keep']);
    } finally {
      await ctx.close();
    }
  });
});

describe('require.resolve', () => {
  test('answers relative to the file that wrote the call', async ({}, testInfo) => {
    // Both helper and caller are bundled into ONE module, so a per-file
    // answer can only come from the source map — which is the point.
    const dir = testInfo.outputPath('rr');
    await fs.promises.mkdir(dir, { recursive: true });
    await fs.promises.writeFile(`${dir}/sibling.ts`, 'export const x = 1;\n');

    const resolved = require.resolve(`${dir}/sibling.ts`);
    expect(resolved.endsWith('sibling.ts')).toBe(true);
    expect(fs.existsSync(resolved)).toBe(true);
  });

  test('appends an extension and answers a builtin with itself', async ({}, testInfo) => {
    const dir = testInfo.outputPath('rr2');
    await fs.promises.mkdir(dir, { recursive: true });
    await fs.promises.writeFile(`${dir}/leaf.ts`, 'export const y = 2;\n');

    // No extension in the specifier; Node appends one.
    expect(require.resolve(`${dir}/leaf`).endsWith('leaf.ts')).toBe(true);
    // A builtin resolves to its own name, as in Node.
    expect(require.resolve('fs')).toBe('fs');
    expect(require.resolve('node:path')).toBe('node:path');
  });

  test("throws Node's message when nothing resolves", async ({}) => {
    let message = '';
    try {
      require.resolve('./definitely-not-here');
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message.includes("Cannot find module './definitely-not-here'")).toBe(true);
  });
});
