// Ported from Playwright's page-set-content.spec.ts: content() is the
// doctype plus the document element, and includeShadow serializes open
// shadow roots as declarative templates.
import { test, expect } from '@ferridriver/test';

const html = '<!DOCTYPE html><html lang="en"><head></head><body><div id="host"><template shadowrootmode="open"><div id="nested"><template shadowrootmode="open"><span>nested</span></template><slot></slot></div></template><span>light</span></div><div id="closed"></div></body></html>';

test('includes open shadow roots when asked', async ({ page }) => {
  await page.setContent(html.replace('<div id="closed">', '<div id="closed"><template shadowrootmode="closed"><span>closed</span></template>'));
  expect(await page.content()).toBe('<!DOCTYPE html><html lang="en"><head></head><body><div id="host"><span>light</span></div><div id="closed"></div></body></html>');
  expect(await page.content({ includeShadow: true })).toBe(html);
  expect(await page.mainFrame().content({ includeShadow: true })).toBe(html);
});

test('a frame keeps its doctype', async ({ page }) => {
  await page.setContent('<iframe srcdoc="<!DOCTYPE html><p>child</p>"></iframe>');
  const child = page.frames()[1];
  await child.waitForSelector('p');
  expect(await child.content()).toBe('<!DOCTYPE html><html><head></head><body><p>child</p></body></html>');
});
