// `page.pickLocator()` — the selector for whatever the reader clicks.
//
// The recorder answers "what did I just do"; this answers "what do I
// call that", which is the question left over when a selector has to go
// into code someone already wrote. `ferridriver codegen --pick-locator`
// is the same thing from a terminal.
//
// A test cannot wave a mouse, so the click is dispatched through the
// input protocol. It has to be: the picker ignores anything whose
// `isTrusted` is false, which is exactly what a page-side `.click()`
// produces. What comes back is a real generated selector rather than a
// repeat of whatever the test asked for.

import { test, describe, expect } from '@ferridriver/test';

const PAGE = `<html><body>
  <main>
    <button id="save" data-testid="save-button">Save</button>
    <button id="cancel">Cancel</button>
  </main>
</body></html>`;

describe('page.pickLocator', () => {
  test('resolves with a locator for the element that was clicked', async ({ page }) => {
    await page.setContent(PAGE);

    // Where to click, read before the picker starts swallowing events.
    const box = await page.locator('#save').boundingBox();
    expect(box !== null).toBe(true);

    // Armed first, then clicked: the picker installs its listener when
    // the call starts, so clicking before it is armed picks nothing.
    const picking = page.pickLocator();
    await page.waitForFunction('window.__fdPickerReady === true');
    await page.mouse.click(box!.x + box!.width / 2, box!.y + box!.height / 2);

    const picked = await picking;
    // The picker's own generator chose this, and it preferred the test
    // id over the element id: a selector the test never mentioned.
    expect(picked.selector.includes('save-button')).toBe(true);
    expect(await picked.textContent()).toBe('Save');

    // And it is a working locator, not a string that reads like one.
    await expect(picked).toHaveText('Save');
  });

  test('cancelling ends the wait without a selection', async ({ page }) => {
    await page.setContent(PAGE);

    const picking = page.pickLocator();
    await page.waitForFunction('window.__fdPickerReady === true');
    await page.cancelPickLocator();

    let message = '';
    try {
      await picking;
    } catch (e) {
      message = String(e);
    }
    expect(message.length).toBeGreaterThan(0);
    // The page-side state is gone too, so a second pick starts clean.
    expect(await page.evaluate('window.__fdPicker === true')).toBe(false);
  });
});
