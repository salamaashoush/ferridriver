// Ported from Playwright's page-dialog.spec.ts, browsercontext-events
// .spec.ts and chromium.spec.ts: 'dialogclosed' fires once, with the
// same Dialog the 'dialog' event handed over, however the dialog closed.
import { test, expect } from '@ferridriver/test';
import type { Dialog } from '@ferridriver/test';

test('fires dialogclosed when the dialog is accepted', async ({ page }) => {
  const closed: Dialog[] = [];
  page.on('dialogclosed', dialog => closed.push(dialog as Dialog));
  let opened: Dialog | undefined;
  page.on('dialog', dialog => {
    opened = dialog as Dialog;
    void opened.accept();
  });
  await page.evaluate(() => alert('yo'));
  await expect.poll(() => closed.length).toBe(1);
  expect(closed[0] === opened).toBe(true);
  await page.evaluate(() => 1);
  await page.evaluate(() => 1);
  expect(closed.length).toBe(1);
});

test('fires dialogclosed when the dialog is dismissed', async ({ page }) => {
  const closedPromise = page.waitForEvent('dialogclosed');
  page.on('dialog', dialog => void (dialog as Dialog).dismiss());
  await page.evaluate(() => confirm('boolean?'));
  const dialog = await closedPromise as Dialog;
  expect(dialog.type()).toBe('confirm');
  expect(dialog.message()).toBe('boolean?');
});

test('fires dialogclosed for auto-dismissed dialogs', async ({ page }) => {
  const closedPromise = page.waitForEvent('dialogclosed');
  await page.evaluate(() => alert('yo'));
  const dialog = await closedPromise as Dialog;
  expect(dialog.message()).toBe('yo');
});

test('fires dialogclosed on the page and its context', async ({ page, context }) => {
  const promise = page.evaluate(() => prompt('hey?'));
  const dialog = await page.waitForEvent('dialog') as Dialog;
  const [closedOnContext, closedOnPage] = await Promise.all([
    context.waitForEvent('dialogclosed'),
    page.waitForEvent('dialogclosed'),
    dialog.accept('hello'),
  ]);
  expect(closedOnContext === dialog).toBe(true);
  expect(closedOnPage === dialog).toBe(true);
  expect(await promise).toBe('hello');
});

test('a second accept names the verb that was refused', async ({ page }) => {
  page.on('dialog', async dialog => {
    const d = dialog as Dialog;
    await d.dismiss();
    const error = await d.dismiss().catch((e: Error) => e);
    expect(String(error)).toContain('Cannot dismiss dialog which is already handled!');
  });
  await page.evaluate(() => confirm('twice?'));
});

test.describe('closed by the browser', () => {
  test.skip(({ browserName }) => browserName !== 'chromium', 'a raw CDP session closes the dialog behind Playwright');
  test('fires dialogclosed when the dialog is closed out of band', async ({ page, context }) => {
    const client = await context.newCDPSession(page);
    await client.send('Page.enable');
    const dialogPromise = page.waitForEvent('dialog');
    const closedPromise = page.waitForEvent('dialogclosed');
    const evaluatePromise = page.evaluate(() => alert('yo'));
    const dialog = await dialogPromise;
    await client.send('Page.handleJavaScriptDialog', { accept: true });
    expect((await closedPromise) === dialog).toBe(true);
    await evaluatePromise;
  });
});
