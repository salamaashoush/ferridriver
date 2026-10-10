// Ported from Playwright's tests/page/page-evaluate-callback.spec.ts and
// page-add-init-script-callback.spec.ts: with `exposeFunctions`, a
// function inside the argument reaches the page as a callable that runs
// the caller's function and settles with its result.
import { test, expect } from '@ferridriver/test';
import type { Frame, Page } from '@ferridriver/test';

type Callback = (...args: any[]) => any;
type Callbacks = Record<string, Callback>;

const ORIGIN = 'http://127.0.0.1:47831';
const CROSS_ORIGIN = 'http://localhost:47831';
const EMPTY_PAGE = `${ORIGIN}/evaluate-callback/empty.html`;

async function serveEmpty(page: Page) {
  await page.route('**/evaluate-callback/**', route => route.fulfill({ body: '<!doctype html><body></body>', contentType: 'text/html' }));
}

async function attachFrame(page: Page, id: string, url: string): Promise<Frame> {
  const handle = await page.evaluateHandle(async ({ id, url }: { id: string; url: string }) => {
    const frame = document.createElement('iframe');
    frame.id = id;
    frame.src = url;
    document.body.appendChild(frame);
    await new Promise(resolve => (frame.onload = resolve));
    return frame;
  }, { id, url });
  return (await handle.asElement()!.contentFrame())!;
}

test.describe('evaluate with exposeFunctions', () => {
  test('throws without the exposeFunctions option', async ({ page }) => {
    const error = await page.evaluate(({ cb }: Callbacks) => cb(), { cb: () => {} }).catch((e: Error) => e) as Error;
    expect(error.message).toMatch(/Attempting to serialize unexpected value at position "cb": \(\) => {}/);
  });

  test('calls a function passed as an argument', async ({ page }) => {
    const received: number[] = [];
    await page.evaluate(async ({ cb }: Callbacks) => {
      await cb(1);
      await cb(2);
    }, { cb: (n: number) => { received.push(n); } }, { exposeFunctions: true });
    expect(received).toEqual([1, 2]);
  });

  test('accepts a function as the whole argument', async ({ page }) => {
    const received: string[] = [];
    await page.evaluate(async (cb: Callback) => {
      await cb('a');
      await cb('b');
    }, async (s: string) => { received.push(s); }, { exposeFunctions: true });
    expect(received).toEqual(['a', 'b']);
  });

  test('passes arguments to the callback', async ({ page }) => {
    const args = await new Promise<unknown[]>(resolve => {
      page.evaluate(({ cb }: Callbacks) => cb(1, 'two', { three: 3 }, [4]), {
        cb: (...a: unknown[]) => resolve(a),
      }, { exposeFunctions: true }).catch(() => {});
    });
    expect(args).toEqual([1, 'two', { three: 3 }, [4]]);
  });

  test('returns the callback result to the page', async ({ page }) => {
    const doubled = await page.evaluate(async ({ cb }: Callbacks) => await cb(21), {
      cb: async (n: number) => n * 2,
    }, { exposeFunctions: true });
    expect(doubled).toBe(42);
  });

  test('awaits an async callback result', async ({ page }) => {
    const value = await page.evaluate(async ({ cb }: Callbacks) => await cb(20), {
      cb: async (n: number) => { await new Promise(f => setTimeout(f, 10)); return n + 1; },
    }, { exposeFunctions: true });
    expect(value).toBe(21);
  });

  test('propagates callback errors to the page', async ({ page }) => {
    const message = await page.evaluate(async ({ cb }: Callbacks) => {
      try {
        await cb();
        return 'no error';
      } catch (e) {
        return (e as Error).message;
      }
    }, { cb: async () => { throw new Error('boom'); } }, { exposeFunctions: true });
    expect(message).toContain('boom');
  });

  test('works with a fire-and-forget setTimeout callback', async ({ page }) => {
    const value = await new Promise<number>(resolve => {
      page.evaluate(({ cb }: Callbacks) => { setTimeout(() => cb(5), 0); }, {
        cb: (n: number) => resolve(n),
      }, { exposeFunctions: true }).catch(() => {});
    });
    expect(value).toBe(5);
  });

  test('supports multiple callbacks', async ({ page }) => {
    const result = await page.evaluate(async ({ add, mul }: Callbacks) => {
      return (await add(2, 3)) + (await mul(2, 3));
    }, {
      add: async (a: number, b: number) => a + b,
      mul: async (a: number, b: number) => a * b,
    }, { exposeFunctions: true });
    expect(result).toBe(11);
  });

  test('works with evaluateHandle', async ({ page }) => {
    const received: number[] = [];
    const handle = await page.evaluateHandle(async ({ cb }: Callbacks) => {
      await cb(7);
      return { done: true };
    }, { cb: async (n: number) => { received.push(n); } }, { exposeFunctions: true });
    expect(await handle.jsonValue()).toEqual({ done: true });
    expect(received).toEqual([7]);
  });

  test('works in a child frame', async ({ page }) => {
    await serveEmpty(page);
    await page.goto(EMPTY_PAGE);
    const frame = await attachFrame(page, 'frame1', EMPTY_PAGE);
    const received: number[] = [];
    await frame.evaluate(async ({ cb }: Callbacks) => { await cb(42); }, { cb: async (n: number) => { received.push(n); } }, { exposeFunctions: true });
    expect(received).toEqual([42]);
  });

  test('routes callbacks back to the calling frame', async ({ page }) => {
    await serveEmpty(page);
    await page.goto(EMPTY_PAGE);
    const frame = await attachFrame(page, 'frame1', EMPTY_PAGE);
    const greet = async (where: string) => `hello ${where}`;
    const [fromMain, fromChild] = await Promise.all([
      page.evaluate(async ({ cb }: Callbacks) => await cb('main'), { cb: greet }, { exposeFunctions: true }),
      frame.evaluate(async ({ cb }: Callbacks) => await cb('child'), { cb: greet }, { exposeFunctions: true }),
    ]);
    expect(fromMain).toBe('hello main');
    expect(fromChild).toBe('hello child');
  });

  test('works with jsHandle.evaluate', async ({ page }) => {
    const handle = await page.evaluateHandle(() => window);
    const received: number[] = [];
    await handle.evaluate(async (_win: Window, { cb }: Callbacks) => { await cb(99); }, { cb: async (n: number) => { received.push(n); } }, { exposeFunctions: true });
    expect(received).toEqual([99]);
  });

  test('works with locator.evaluate', async ({ page }) => {
    await page.setContent('<div id=target>hello</div>');
    const received: string[] = [];
    await page.locator('#target').evaluate(async (element: Element, { cb }: Callbacks) => { await cb(element.id); }, { cb: async (s: string) => { received.push(s); } }, { exposeFunctions: true });
    expect(received).toEqual(['target']);
  });

  test('returns the callback result with locator.evaluate', async ({ page }) => {
    await page.setContent('<div id=target>7</div>');
    const result = await page.locator('#target').evaluate(async (element: Element, { double }: Callbacks) => {
      return await double(+element.textContent!);
    }, { double: async (n: number) => n * 2 }, { exposeFunctions: true });
    expect(result).toBe(14);
  });

  test('works with locator.evaluateHandle', async ({ page }) => {
    await page.setContent('<div id=target>hello</div>');
    const received: string[] = [];
    const handle = await page.locator('#target').evaluateHandle(async (element: Element, { cb }: Callbacks) => {
      await cb(element.id);
      return element;
    }, { cb: async (s: string) => { received.push(s); } }, { exposeFunctions: true });
    expect(received).toEqual(['target']);
    expect(await handle.evaluate((element: Element) => element.id)).toBe('target');
  });

  test('works with locator.evaluate inside an iframe', async ({ page }) => {
    await serveEmpty(page);
    await page.goto(EMPTY_PAGE);
    const frame = await attachFrame(page, 'frame1', EMPTY_PAGE);
    await frame.evaluate(() => { document.body.innerHTML = '<div id=target>in-frame</div>'; });
    const received: (string | null)[] = [];
    await page.frameLocator('#frame1').locator('#target').evaluate(async (element: Element, { cb }: Callbacks) => {
      await cb(element.textContent);
    }, { cb: async (text: string | null) => { received.push(text); } }, { exposeFunctions: true });
    expect(received).toEqual(['in-frame']);
  });

  test('survives a navigation and keeps working', async ({ page }) => {
    await serveEmpty(page);
    const received: number[] = [];
    await page.evaluate(async ({ cb }: Callbacks) => { await cb(1); }, { cb: async (n: number) => { received.push(n); } }, { exposeFunctions: true });
    await page.goto(EMPTY_PAGE);
    await page.evaluate(async ({ cb }: Callbacks) => { await cb(2); }, { cb: async (n: number) => { received.push(n); } }, { exposeFunctions: true });
    expect(received).toEqual([1, 2]);
  });

  test('does not register the callback on the global object', async ({ page }) => {
    const result = await page.evaluate(async ({ cb }: Callbacks) => {
      await cb();
      return Object.getOwnPropertyNames(globalThis).filter(name => name.startsWith('__pw_fn_'));
    }, { cb: async () => {} }, { exposeFunctions: true });
    expect(result).toEqual([]);
  });

  test('scopes the page-side callback to the execution context', async ({ page }) => {
    await serveEmpty(page);
    await page.evaluate(({ cb }: Callbacks) => { (window as any).__cb = cb; }, { cb: () => {} }, { exposeFunctions: true });
    expect(await page.evaluate(() => typeof (window as any).__cb)).toBe('function');
    await page.goto(EMPTY_PAGE);
    expect(await page.evaluate(() => typeof (window as any).__cb)).toBe('undefined');
  });
});

test.describe('addInitScript with exposeFunctions', () => {
  test('drops functions without the exposeFunctions option', async ({ page }) => {
    await serveEmpty(page);
    await page.addInitScript(({ cb }: Callbacks) => {
      (window as any).cbType = typeof cb;
    }, { cb: () => {} });
    await page.goto(EMPTY_PAGE);
    expect(await page.evaluate(() => (window as any).cbType)).toBe('undefined');
  });

  test('throws when the script is not a function', async ({ page }) => {
    const error = await page.addInitScript({ content: 'window.foo = 1;' }, undefined, { exposeFunctions: true }).catch((e: Error) => e) as Error;
    expect(error.message).toContain('Passing functions requires the init script to be a function');
  });

  test('calls a function passed as an argument', async ({ page }) => {
    await serveEmpty(page);
    const received: number[] = [];
    await page.addInitScript(async ({ cb }: Callbacks) => {
      await cb(1);
      await cb(2);
    }, { cb: async (n: number) => { received.push(n); } }, { exposeFunctions: true });
    await page.goto(EMPTY_PAGE);
    await expect.poll(() => received).toEqual([1, 2]);
  });

  test('accepts a function as the whole argument', async ({ page }) => {
    await serveEmpty(page);
    await page.addInitScript(async (cb: Callback) => {
      (window as any).result = await cb('a');
    }, async (s: string) => s + 'b', { exposeFunctions: true });
    await page.goto(EMPTY_PAGE);
    await expect.poll(() => page.evaluate(() => (window as any).result)).toBe('ab');
  });

  test('propagates callback errors to the page', async ({ page }) => {
    await serveEmpty(page);
    await page.addInitScript(async ({ cb }: Callbacks) => {
      try {
        await cb();
        (window as any).result = 'no error';
      } catch (e) {
        (window as any).result = (e as Error).message;
      }
    }, { cb: async () => { throw new Error('boom'); } }, { exposeFunctions: true });
    await page.goto(EMPTY_PAGE);
    await expect.poll(async () => String(await page.evaluate(() => (window as any).result)).includes('boom')).toBe(true);
  });

  test('passes rich values alongside the functions', async ({ page }) => {
    await serveEmpty(page);
    await page.addInitScript(async ({ cb, when, re, big, nested }: any) => {
      (window as any).result = await cb([when instanceof Date, re.test('ABC'), typeof big, nested.self === nested]);
    }, (() => {
      const nested: Record<string, unknown> = {};
      nested.self = nested;
      return { cb: async (v: unknown) => v, when: new Date(0), re: /abc/i, big: 10n, nested };
    })(), { exposeFunctions: true });
    await page.goto(EMPTY_PAGE);
    await expect.poll(() => page.evaluate(() => (window as any).result)).toEqual([true, true, 'bigint', true]);
  });

  test('survives a navigation and keeps working', async ({ page }) => {
    await serveEmpty(page);
    await page.route(`${CROSS_ORIGIN}/**`, route => route.fulfill({ body: '<!doctype html><body></body>', contentType: 'text/html' }));
    const received: number[] = [];
    await page.addInitScript(({ cb }: Callbacks) => {
      (window as any).cb = cb;
    }, { cb: (n: number) => { received.push(n); return n * 2; } }, { exposeFunctions: true });
    await page.goto(EMPTY_PAGE);
    expect(await page.evaluate(() => (window as any).cb(1))).toBe(2);
    await page.goto(`${CROSS_ORIGIN}/evaluate-callback/empty.html`);
    expect(await page.evaluate(() => (window as any).cb(2))).toBe(4);
    expect(received).toEqual([1, 2]);
  });

  test('works in a child frame', async ({ page }) => {
    await serveEmpty(page);
    await page.route('**/evaluate-callback/one-frame.html', route => route.fulfill({ body: '<iframe src="empty.html"></iframe>', contentType: 'text/html' }));
    const received: number[] = [];
    await page.addInitScript(async ({ cb }: Callbacks) => {
      await cb(42);
    }, { cb: async (n: number) => { received.push(n); } }, { exposeFunctions: true });
    await page.goto(`${ORIGIN}/evaluate-callback/one-frame.html`);
    await expect.poll(() => received).toEqual([42, 42]);
  });

  test('does not register the callback on the global object', async ({ page }) => {
    await serveEmpty(page);
    await page.addInitScript(async ({ cb }: Callbacks) => {
      await cb();
      (window as any).result = Object.getOwnPropertyNames(globalThis).filter(name => name.startsWith('__pw_fn_'));
    }, { cb: async () => {} }, { exposeFunctions: true });
    await page.goto(EMPTY_PAGE);
    await expect.poll(() => page.evaluate(() => (window as any).result)).toEqual([]);
  });

  test('context init script functions reach new pages until disposed', async ({ browser }) => {
    const context = await browser.newContext();
    try {
      await context.route('**/evaluate-callback/**', route => route.fulfill({ body: '<!doctype html><body></body>', contentType: 'text/html' }));
      const received: string[] = [];
      const disposable = await context.addInitScript(({ cb, mul }: Callbacks) => {
        (window as any).mul = mul;
        void cb(location.href);
      }, { cb: async (href: string) => { received.push(href); }, mul: (a: number, b: number) => a * b }, { exposeFunctions: true });
      const page = await context.newPage();
      await page.goto(EMPTY_PAGE);
      await expect.poll(() => received.includes(EMPTY_PAGE)).toBe(true);
      expect(await page.evaluate(() => (window as any).mul(6, 7))).toBe(42);
      await disposable.dispose();
      await page.goto(EMPTY_PAGE);
      expect(await page.evaluate(() => typeof (window as any).mul)).toBe('undefined');
    } finally {
      await context.close();
    }
  });
});
