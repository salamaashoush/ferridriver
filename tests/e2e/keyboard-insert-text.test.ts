// keyboard.insertText puts text in at the focus with no key events, on
// every engine. Firefox can only press keys, so there the text goes in from
// a page script, and the page sees one untrusted `input` event carrying it.

import { test, describe, expect } from '@ferridriver/test';

const LOG =
  '<script>window.log = [];' +
  "for (const t of ['keydown', 'keypress', 'keyup', 'beforeinput', 'input']) document.addEventListener(t, e => " +
  "log.push([t, e.target.id || e.target.nodeName, e.isTrusted, e.data == null ? '' : e.data].join('|')), true);" +
  '</script>';

describe('keyboard.insertText', () => {
  test('inserts at the caret and over the selection, with no key events', async ({ page, browserName }) => {
    await page.setContent(`<input id=i value=ab><input id=s value=abcd>${LOG}`);
    await page.focus('#i');
    await page.evaluate("document.getElementById('i').setSelectionRange(1, 1); window.log = []");
    await page.keyboard.insertText('héllo');
    expect(await page.inputValue('#i')).toBe('ahéllob');

    await page.focus('#s');
    await page.evaluate("document.getElementById('s').setSelectionRange(1, 3)");
    await page.keyboard.insertText('X');
    expect(await page.inputValue('#s')).toBe('aXd');

    const log = (await page.evaluate('window.log')) as string[];
    expect(log.filter((e) => e.startsWith('key'))).toEqual([]);
    const trusted = browserName !== 'firefox';
    expect(log.filter((e) => e.startsWith('input|'))).toEqual([`input|i|${trusted}|héllo`, `input|s|${trusted}|X`]);
  });

  test('inserts a line break in a contenteditable', async ({ page }) => {
    await page.setContent(`<div id=c contenteditable></div>${LOG}`);
    await page.focus('#c');
    await page.keyboard.insertText('top\nbottom');
    expect(await page.locator('#c').innerText()).toMatch(/^top\n+bottom$/);
    const log = (await page.evaluate('window.log')) as string[];
    expect(log.filter((e) => e.startsWith('key'))).toEqual([]);
  });

  test('reaches the field focused inside a frame', async ({ page }) => {
    await page.setContent(`<iframe id=f srcdoc="<input id=inner>${LOG.replace(/"/g, '&quot;')}"></iframe>`);
    const frame = page.frameLocator('#f');
    await frame.locator('#inner').focus();
    await page.keyboard.insertText('deep');
    await expect(frame.locator('#inner')).toHaveValue('deep');
    const inner = page.frames().find((f) => f !== page.mainFrame());
    const log = (await inner!.evaluate('window.log')) as string[];
    expect(log.filter((e) => e.startsWith('key'))).toEqual([]);
    expect(log.filter((e) => e.startsWith('input|')).length).toBe(1);
  });
});
