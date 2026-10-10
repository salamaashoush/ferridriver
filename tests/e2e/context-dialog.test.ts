// Ported from Playwright's tests/library/browsercontext-events.spec.ts: the
// context hears every page's dialogs, popups' included, and a context
// listener alone keeps a dialog open for it to handle.
import { test, expect } from '@ferridriver/test';
import type { Dialog, Page } from '@ferridriver/test';

test.describe('context dialog event', () => {
  test('works', async ({ page }) => {
    const promise = page.evaluate(() => prompt('hey?'));
    const [dialog1, dialog2] = await Promise.all([
      page.context().waitForEvent('dialog') as Promise<Dialog>,
      page.waitForEvent('dialog') as Promise<Dialog>,
    ]);
    expect(dialog1).toBe(dialog2);
    expect(dialog1.message()).toBe('hey?');
    expect(dialog1.page()).toBe(page);
    await dialog1.accept('hello');
    expect(await promise).toBe('hello');
  });

  test('dialogclosed follows it', async ({ page }) => {
    const promise = page.evaluate(() => prompt('hey?'));
    const dialog = await page.context().waitForEvent('dialog') as Dialog;
    const [closed1, closed2] = await Promise.all([
      page.context().waitForEvent('dialogclosed'),
      page.waitForEvent('dialogclosed'),
      dialog.accept('hello'),
    ]);
    expect(closed1).toBe(dialog);
    expect(closed2).toBe(dialog);
    expect(await promise).toBe('hello');
  });

  test('a context listener alone holds the dialog open', async ({ page }) => {
    const seen: string[] = [];
    page.context().on('dialog', (dialog: unknown) => {
      seen.push((dialog as Dialog).message());
      void (dialog as Dialog).accept('from context');
    });
    expect(await page.evaluate(() => prompt('who?'))).toBe('from context');
    expect(seen).toEqual(['who?']);
  });

  test('works in a popup', async ({ page }) => {
    const promise = page.evaluate(() => {
      const win = window.open('')!;
      return (win as any).prompt('hey?');
    });
    const [dialog, popup] = await Promise.all([
      page.context().waitForEvent('dialog') as Promise<Dialog>,
      page.waitForEvent('popup') as Promise<Page>,
    ]);
    expect(dialog.message()).toBe('hey?');
    expect(dialog.page()).toBe(popup);
    await dialog.accept('hello');
    expect(await promise).toBe('hello');
  });

  test('works in an immediately closed popup', async ({ page }) => {
    const promise = page.evaluate(() => {
      const win = window.open()!;
      const result = (win as any).prompt('hey?');
      win.close();
      return result;
    });
    const [dialog, popup] = await Promise.all([
      page.context().waitForEvent('dialog') as Promise<Dialog>,
      page.waitForEvent('popup') as Promise<Page>,
    ]);
    expect(dialog.message()).toBe('hey?');
    expect(dialog.page()).toBe(popup);
    await dialog.accept('hello');
    expect(await promise).toBe('hello');
  });
});
