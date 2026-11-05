// keyboard.insertText puts text in at the focus with no key events, on
// every engine, and the page sees one trusted `input` event carrying it.
// Firefox has no command that types text, so there it goes in through the
// editing command, which the page cannot tell from typing.

import { test, describe, expect } from '@ferridriver/test';

const LOG =
  '<script>window.log = [];' +
  "for (const t of ['keydown', 'keypress', 'keyup', 'beforeinput', 'input']) document.addEventListener(t, e => " +
  "log.push([t, e.target.id || e.target.nodeName, e.isTrusted, e.data == null ? '' : e.data].join('|')), true);" +
  '</script>';

describe('keyboard.insertText', () => {
  test('inserts at the caret and over the selection, with no key events', async ({ page }) => {
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
    expect(log.filter((e) => e.startsWith('input|'))).toEqual(['input|i|true|héllo', 'input|s|true|X']);
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

describe('fill through inserted text', () => {
  test('reaches a field whose framework tracks the value it set, and commits on leaving', async ({ page }) => {
    // The way React tracks a controlled input: the element's own `value`
    // property records each value the code sets, and only a value it did
    // not set is a change.
    await page.setContent(
      '<input id=r><p id=state></p><input id=other><script>' +
        "const input = document.getElementById('r');" +
        "const native = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');" +
        "let tracked = '';" +
        "Object.defineProperty(input, 'value', { configurable: true, get() { return native.get.call(this); }, set(v) { tracked = String(v); native.set.call(this, v); } });" +
        "input.addEventListener('input', () => { if (input.value !== tracked) { tracked = input.value; document.getElementById('state').textContent = 'changed:' + input.value; } });" +
        "window.commits = []; input.addEventListener('change', () => commits.push(input.value));" +
        '</script>',
    );
    await page.locator('#r').fill('sashoush');
    await expect(page.locator('#state')).toHaveText('changed:sashoush');
    expect(await page.evaluate('window.commits')).toEqual([]);
    await page.locator('#other').focus();
    expect(await page.evaluate('window.commits')).toEqual(['sashoush']);
  });
});
