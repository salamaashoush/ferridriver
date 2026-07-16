// Ported from crates/ferridriver-cli/tests/backends_support/script_input.rs —
// page-level interaction: click/fill/type/press/hover/dblclick,
// selectOption, check/uncheck, scrolling, auto-scroll clicks, dialog
// survival, and fill's event dispatch. Test titles mirror the original
// Rust fn names.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('input', () => {
  test('script_click', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<h1 id='h'>Before</h1><button id='btn' onclick=\"document.getElementById('h').textContent='After'\">Go</button>",
      ),
    );
    await page.click('#btn');
    expect(await page.textContent('#h')).toBe('After');
  });

  test('script_fill', async ({ page }) => {
    await page.goto(dataUrl("<input id='i' type='text'>"));
    await page.fill('#i', 'Alice');
    expect(await page.inputValue('#i')).toBe('Alice');
  });

  test('script_fill_form', async ({ page }) => {
    await page.goto(dataUrl("<input id='a'><input id='b'>"));
    await page.fill('#a', 'val1');
    await page.fill('#b', 'val2');
    expect(await page.inputValue('#a')).toBe('val1');
    expect(await page.inputValue('#b')).toBe('val2');
  });

  test('script_type', async ({ page }) => {
    await page.goto(dataUrl("<input id='i' type='text'>"));
    await page.locator('#i').click();
    await page.type('#i', 'Bob');
    expect(await page.inputValue('#i')).toBe('Bob');
  });

  test('script_press', async ({ page }) => {
    await page.goto(dataUrl("<textarea id='t'></textarea>"));
    await page.locator('#t').click();
    await page.press('#t', 'Enter');
    expect((await page.inputValue('#t')).length).toBeGreaterThan(0);
  });

  test('script_hover', async ({ page }) => {
    await page.goto(
      dataUrl("<div id='d' onmouseenter=\"this.textContent='hovered'\" style='width:100px;height:100px'>hover me</div>"),
    );
    await page.locator('#d').hover();
    expect(await page.textContent('#d')).toBe('hovered');
  });

  test('script_dblclick', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<h1 id='h'>0</h1><button id='b' onclick=\"document.getElementById('h').textContent=Number(document.getElementById('h').textContent)+1\">+</button>",
      ),
    );
    await page.dblclick('#b');
    expect(await page.textContent('#h')).toBe('2');
  });

  test('script_select_option', async ({ page }) => {
    await page.goto(
      dataUrl("<select id='s'><option value='apple'>Apple</option><option value='banana'>Banana</option></select>"),
    );
    await page.selectOption('#s', 'banana');
    expect(await page.inputValue('#s')).toBe('banana');
  });

  test('script_check_uncheck', async ({ page }) => {
    await page.goto(dataUrl("<input id='c' type='checkbox'>"));
    await page.check('#c');
    expect(await page.isChecked('#c')).toBe(true);
    await page.uncheck('#c');
    expect(await page.isChecked('#c')).toBe(false);
  });

  test('script_scroll', async ({ page }) => {
    await page.goto(dataUrl("<div style='height:3000px'>tall</div>"));
    await page.evaluate('window.scrollBy(0, 500)');
    expect((await page.evaluate('window.scrollY')) as number).toBeGreaterThan(0);
  });

  test('script_scroll_into_view', async ({ page }) => {
    await page.goto(dataUrl("<div style='height:3000px'></div><div id='bottom'>bottom</div>"));
    await page.locator('#bottom').scrollIntoViewIfNeeded();
    expect((await page.evaluate('window.scrollY')) as number).toBeGreaterThan(100);
  });

  test('script_click_offscreen', async ({ page }) => {
    await page.goto(
      dataUrl("<div style='height:3000px'></div><button id='b' onclick=\"this.textContent='clicked'\">far</button>"),
    );
    await page.click('#b');
    expect(await page.textContent('#b')).toBe('clicked');
  });

  test('script_dialog_alert', async ({ page }) => {
    // Dialogs are auto-dismissed; the click must not hang.
    await page.goto(dataUrl("<button id='b' onclick=\"alert('hello')\">Go</button>"));
    await page.click('#b');
  });

  test('script_fill_dispatches_events', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<input id='i' type='text'><div id='r'></div>" +
          "<script>document.getElementById('i').addEventListener('change', function(e) { document.getElementById('r').textContent = 'changed:' + e.target.value; });</script>",
      ),
    );
    await page.fill('#i', 'test');
    expect(await page.textContent('#r')).toBe('changed:test');
  });
});
