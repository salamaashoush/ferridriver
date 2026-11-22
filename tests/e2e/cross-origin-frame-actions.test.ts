// A pointer action on an element inside a cross-origin iframe lands on
// that element. The injected script places a click point by walking
// `frameElement` up to the top document, and a cross-origin frame's
// `frameElement` cannot be read, so each such boundary adds its
// `<iframe>`'s content box as Playwright's `crPage._framePosition` does
// (/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts).
// `localhost` and `127.0.0.1` are different origins, so the inner
// documents below are cross-origin to the page.

import { describe, expect, test } from '@ferridriver/test';
import type { Locator, Page } from '@ferridriver/test';

const BUTTON = `<button id="go" style="margin: 30px 0 0 60px"
  onclick="window.clicked = (window.clicked || 0) + 1"
  onmouseenter="window.hovered = true">Go</button>`;

/** Add an offset, bordered, padded iframe to the document `host` is in. */
async function addFrame(host: Locator, name: string, src: string) {
  await host.evaluate(
    (_body: HTMLElement, [name, src]: string[]) => {
      const frame = document.createElement('iframe');
      frame.name = name;
      frame.src = src;
      frame.style.cssText =
        'position: absolute; left: 120px; top: 90px; width: 400px; height: 300px; border: 7px solid black; padding: 5px';
      document.body.appendChild(frame);
      return new Promise((resolve) => (frame.onload = resolve));
    },
    [name, src],
  );
}

async function page127(page: Page) {
  await page.goto('http://127.0.0.1:47831/fx/landed');
  return page.locator('body');
}

describe('pointer actions in a cross-origin iframe', () => {
  test('click and hover reach the element', async ({ page }) => {
    await addFrame(await page127(page), 'remote', 'http://localhost:47831/fx/landed');
    const remote = page.frameLocator('iframe[name="remote"]');
    await remote.locator('body').evaluate((body: HTMLElement, html: string) => {
      body.innerHTML = html;
    }, BUTTON);
    const button = remote.getByRole('button', { name: 'Go' });
    await button.click({ timeout: 5000 });
    await button.hover({ timeout: 5000 });
    expect(await remote.locator('body').evaluate(() => [(window as any).clicked, (window as any).hovered])).toEqual([
      1,
      true,
    ]);
  });

  test('a click reaches an element two cross-origin frames down', async ({ page }) => {
    await addFrame(await page127(page), 'outer', 'http://localhost:47831/fx/landed');
    const outer = page.frameLocator('iframe[name="outer"]');
    await addFrame(outer.locator('body'), 'inner', 'http://127.0.0.1:47831/fx/landed?inner');
    const inner = outer.frameLocator('iframe[name="inner"]');
    await inner.locator('body').evaluate((body: HTMLElement, html: string) => {
      body.innerHTML = html;
    }, BUTTON);
    await inner.getByRole('button', { name: 'Go' }).click({ timeout: 5000 });
    expect(await inner.locator('body').evaluate(() => (window as any).clicked)).toBe(1);
  });
});
