// The keyboard keeps the keys it holds between calls: a modifier pressed
// with `keyboard.down` applies to every later key and mouse event until
// `keyboard.up`, exactly as a person holding the key would.

import { test, describe, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

type KeyEntry = [string, string, string, number, number, boolean, boolean, boolean, boolean, boolean];

const LOGGER =
  '<input id=i><div id=d style="width:200px;height:40px">target</div>' +
  '<p id=p>alpha beta gamma</p>' +
  '<script>' +
  'window.log = [];' +
  "for (const t of ['keydown', 'keyup']) document.addEventListener(t, e => log.push([t, e.key, e.code, e.keyCode, e.location, e.repeat, e.ctrlKey, e.shiftKey, e.altKey, e.metaKey]), true);" +
  "for (const t of ['mousedown', 'mouseup', 'click', 'wheel']) document.addEventListener(t, e => log.push([t, e.ctrlKey, e.shiftKey, e.altKey, e.metaKey]), true);" +
  '</script>';

async function open(page: Page): Promise<void> {
  await page.setContent(LOGGER);
  await page.focus('#i');
}

async function keydowns(page: Page): Promise<KeyEntry[]> {
  const log = (await page.evaluate('window.log')) as KeyEntry[];
  return log.filter((e) => e[0] === 'keydown');
}

describe('keyboard state', () => {
  test('a held Shift shifts a key named by its code and sets shiftKey', async ({ page }) => {
    await open(page);
    await page.keyboard.down('Shift');
    await page.keyboard.press('KeyB');
    await page.keyboard.press('Digit1');
    await page.keyboard.up('Shift');
    await page.keyboard.press('KeyB');
    expect(await page.inputValue('#i')).toBe('B!b');
    const downs = (await keydowns(page)).map((e) => [e[1], e[7]]);
    expect(downs).toEqual([
      ['Shift', true],
      ['B', true],
      ['!', true],
      ['b', false],
    ]);
  });

  test('a held ControlOrMeta selects all and the next key replaces the selection', async ({ page }) => {
    await open(page);
    await page.keyboard.type('hello');
    await page.keyboard.down('ControlOrMeta');
    await page.keyboard.press('a');
    await page.keyboard.up('ControlOrMeta');
    await page.keyboard.press('x');
    expect(await page.inputValue('#i')).toBe('x');
  });

  test('a held modifier other than Shift types no text', async ({ page }) => {
    await open(page);
    await page.keyboard.down('Control');
    await page.keyboard.press('m');
    await page.keyboard.up('Control');
    await page.keyboard.press('n');
    expect(await page.inputValue('#i')).toBe('n');
    const downs = (await keydowns(page)).map((e) => [e[1], e[6]]);
    expect(downs).toEqual([
      ['Control', true],
      ['m', true],
      ['n', false],
    ]);
  });

  test('key events report keyCode, code and location', async ({ page }) => {
    await open(page);
    for (const key of ['a', '1', 'Digit5', 'NumpadSubtract', 'ShiftRight', 'ControlRight', 'AltRight', 'MetaRight', 'ControlLeft'])
      await page.keyboard.press(key);
    // Meta's keyCode is the engine's own (Firefox reports 224), so it is
    // left out of the comparison.
    const downs = (await keydowns(page)).map((e) => [e[1], e[2], e[1] === 'Meta' ? null : e[3], e[4]]);
    expect(downs).toEqual([
      ['a', 'KeyA', 65, 0],
      ['1', 'Digit1', 49, 0],
      ['5', 'Digit5', 53, 0],
      ['-', 'NumpadSubtract', 109, 3],
      ['Shift', 'ShiftRight', 16, 2],
      ['Control', 'ControlRight', 17, 2],
      ['Alt', 'AltRight', 18, 2],
      ['Meta', 'MetaRight', null, 2],
      ['Control', 'ControlLeft', 17, 1],
    ]);
  });

  test('a second down of a held key repeats', async ({ page }) => {
    await open(page);
    await page.keyboard.down('a');
    await page.keyboard.down('a');
    await page.keyboard.up('a');
    await page.keyboard.down('a');
    await page.keyboard.up('a');
    expect((await keydowns(page)).map((e) => e[5])).toEqual([false, true, false]);
  });

  test('an unknown key is an error', async ({ page }) => {
    await open(page);
    let message = 'no-throw';
    try {
      await page.keyboard.press('Hyper');
    } catch (e) {
      message = String((e as Error).message ?? e);
    }
    expect(message).toContain('Unknown key: "Hyper"');
  });

  test('a held Shift rides on mouse events', async ({ page }) => {
    await open(page);
    const box = await page.locator('#d').boundingBox();
    if (!box) throw new Error('no box');
    await page.keyboard.down('Shift');
    await page.mouse.move(box.x + 5, box.y + 5);
    await page.mouse.down();
    await page.mouse.up();
    await page.mouse.wheel(0, 10);
    await page.locator('#d').click();
    await page.keyboard.up('Shift');
    await page.mouse.click(box.x + 5, box.y + 5);
    const log = ((await page.evaluate('window.log')) as (string | boolean)[][]).filter((e) => e[0] !== 'keydown' && e[0] !== 'keyup');
    expect(log.map((e) => [e[0], e[2]])).toEqual([
      ['mousedown', true],
      ['mouseup', true],
      ['click', true],
      ['wheel', true],
      ['mousedown', true],
      ['mouseup', true],
      ['click', true],
      ['mousedown', false],
      ['mouseup', false],
      ['click', false],
    ]);
  });

  test('a Shift+click extends the selection', async ({ page }) => {
    await open(page);
    const box = await page.locator('#p').boundingBox();
    if (!box) throw new Error('no box');
    await page.mouse.click(box.x + 1, box.y + box.height / 2);
    await page.keyboard.down('Shift');
    await page.mouse.click(box.x + box.width - 1, box.y + box.height / 2);
    await page.keyboard.up('Shift');
    expect(await page.evaluate('String(window.getSelection())')).toBe('alpha beta gamma');
  });

  test("an action's modifiers replace the held ones and restore them after", async ({ page }) => {
    await open(page);
    await page.keyboard.down('Control');
    await page.locator('#d').click({ modifiers: ['Shift'] });
    await page.keyboard.press('KeyQ');
    await page.keyboard.up('Control');
    const log = (await page.evaluate('window.log')) as (string | boolean)[][];
    const shape = log.map((e) =>
      e[0] === 'keydown' || e[0] === 'keyup' ? [e[0], e[1], e[6], e[7]] : [e[0], e[1], e[2]],
    );
    expect(shape).toEqual([
      ['keydown', 'Control', true, false],
      ['keyup', 'Control', false, false],
      ['keydown', 'Shift', false, true],
      ['mousedown', false, true],
      ['mouseup', false, true],
      ['click', false, true],
      ['keydown', 'Control', true, true],
      ['keyup', 'Shift', true, false],
      ['keydown', 'q', true, false],
      ['keyup', 'q', true, false],
      ['keyup', 'Control', false, false],
    ]);
  });

  test('macOS editing shortcuts move the caret', async ({ page }) => {
    test.skip(process.platform !== 'darwin', 'the shortcuts are the macOS key bindings');
    await open(page);
    await page.keyboard.type('foo bar');
    await page.keyboard.press('Meta+ArrowLeft');
    await page.keyboard.type('X');
    await page.keyboard.press('Alt+ArrowRight');
    await page.keyboard.type('Y');
    expect(await page.inputValue('#i')).toBe('XfooY bar');
  });
});
