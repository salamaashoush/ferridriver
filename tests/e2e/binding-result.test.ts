// What an exposed callback returns reaches the page as an evaluate
// argument does (Playwright `server/page.ts::PageBinding.dispatch`):
// ported from tests/page/page-expose-function.spec.ts "should work with
// handles and complex objects", plus the rich types the same path carries.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

type Callbacks = Record<string, (...args: any[]) => any>;

const FIXTURE = dataUrl("<button id='main'>x</button><iframe srcdoc='<button id=inner>inside</button>'></iframe>");

async function setup(page: Page): Promise<void> {
  await page.goto(FIXTURE);
  await page.waitForSelector('#main');
}

test.describe('exposed callback results', () => {
  test('a handle inside the result is the object it refers to', async ({ page }) => {
    await setup(page);
    const fooHandle = await page.evaluateHandle(() => {
      (window as any).fooValue = { bar: 2 };
      return (window as any).fooValue;
    });
    await page.exposeFunction('handle', () => [{ foo: fooHandle, label: 'kept' }]);
    const seen = await page.evaluate(async () => {
      const [{ foo, label }] = await (window as any).handle();
      return { same: foo === (window as any).fooValue, label };
    });
    expect(seen).toEqual({ same: true, label: 'kept' });
  });

  test('an element handle is the element', async ({ page }) => {
    await setup(page);
    const button = await page.$('#main');
    await page.exposeBinding('pick', async () => button);
    expect(await page.evaluate(async () => (await (window as any).pick()) === document.querySelector('#main'))).toBe(true);
  });

  test('each call takes its own result', async ({ page }) => {
    await setup(page);
    const handles = await Promise.all(
      [0, 1, 2].map(i =>
        page.evaluateHandle((i: number) => (((window as any).slots ??= [])[i] = { i }), i),
      ),
    );
    await page.exposeFunction('slot', (i: number) => handles[i]);
    const seen = await page.evaluate(async () => {
      const got = await Promise.all([2, 0, 1].map(i => (window as any).slot(i)));
      return got.map((value, at) => value === (window as any).slots[[2, 0, 1][at]]);
    });
    expect(seen).toEqual([true, true, true]);
  });

  test('a frame gets a handle of its own document', async ({ page }) => {
    await setup(page);
    const frame = page.frames().find(f => f !== page.mainFrame())!;
    await frame.waitForSelector('#inner');
    const marker = await frame.evaluateHandle(() => ((window as any).marker = { inner: true }));
    await page.exposeFunction('give', () => ({ marker }));
    expect(await frame.evaluate(async () => (await (window as any).give()).marker === (window as any).marker)).toBe(true);
  });

  test('a context function returns a handle to a page opened later', async ({ browser }) => {
    const context = await browser.newContext();
    try {
      let held: unknown = null;
      await context.exposeFunction('held', () => held);
      const page = await context.newPage();
      await setup(page);
      held = await page.evaluateHandle(() => ((window as any).kept = new Map([['k', 1]])));
      expect(await page.evaluate(async () => (await (window as any).held()) === (window as any).kept)).toBe(true);
    } finally {
      await context.close();
    }
  });

  test('a function passed as an argument returns a handle', async ({ page }) => {
    await setup(page);
    const fooHandle = await page.evaluateHandle(() => ((window as any).fooValue = { bar: 2 }));
    const same = await page.evaluate(
      async ({ cb }: Callbacks) => (await cb()) === (window as any).fooValue,
      { cb: () => fooHandle },
      { exposeFunctions: true },
    );
    expect(same).toBe(true);
  });

  test('rich values cross as themselves', async ({ page }) => {
    await setup(page);
    await page.exposeFunction('rich', () => ({
      when: new Date(86400000),
      nan: NaN,
      big: 10n,
      pattern: /a+/gi,
    }));
    await page.exposeFunction('nothing', () => {});
    const seen = await page.evaluate(async () => {
      const rich = await (window as any).rich();
      return {
        when: rich.when instanceof Date && rich.when.getTime(),
        nan: Number.isNaN(rich.nan),
        big: typeof rich.big === 'bigint' && String(rich.big),
        pattern: rich.pattern instanceof RegExp && String(rich.pattern),
        void: (await (window as any).nothing()) === undefined,
      };
    });
    expect(seen).toEqual({ when: 86400000, nan: true, big: '10', pattern: '/a+/gi', void: true });
  });

  test('a function in the result rejects the call', async ({ page }) => {
    await setup(page);
    await page.exposeFunction('makeFunction', () => ({ run: () => 1 }));
    const message = await page.evaluate(() => (window as any).makeFunction().then(() => 'no error', (e: Error) => e.message));
    expect(message).toMatch(/Attempting to serialize unexpected value at position "run"/);
  });
});
