// The recorder against what Playwright's codegen writes for the same pages
// and the same input: real pointer and keyboard input sent to a session's
// pages, recorded over `codegen --attach`, then the spec replayed.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { binary, passed, quote, repo, run, workspace } from './support.mjs';

async function readUntil(name, text) {
  let seen = '';
  while (!seen.includes(text)) {
    const chunk = await commands.read(name, 20000);
    assert.notEqual(chunk, null, `exited before printing ${text}:\n${seen}`);
    seen += chunk + '\n';
  }
  return seen;
}

async function serveAssets() {
  await commands.start('static', {
    binary: join(repo, 'target/debug/ferridriver-fixtures'),
    dir: join(repo, 'tests/assets'),
  });
  const ready = await commands.waitForOutput('static', '\n');
  const match = ready.match(/serving (http:\/\/127\.0\.0\.1:\d+)/);
  assert.ok(match, ready);
  return match[1];
}

// Pointer input the way a person gives it: the pointer moves onto the
// element, then presses and releases.
const helpers = `
const center = async loc => { const b = await loc.boundingBox(); return [b.x + b.width / 2, b.y + b.height / 2]; };
const click = async (p, loc) => { const [x, y] = await center(loc); await p.mouse.move(x, y); await p.mouse.down(); await p.mouse.up(); };
`;

// Record `journey` on a session of `backend` showing `page` (the shop by
// default), check the spec's body is `expected`, and replay the spec.
async function journey(backend, id, script, expected, page = 'shop') {
  const origin = await serveAssets();
  try {
    const { body, cwd } = await record(backend, id, `${origin}/recorder/${page}.html`, script);
    assert.deepEqual(body, expected);
    passed(await run(['test', '--no-inherit', '--headless', 'recorded.spec.ts'], { cwd }));
  } finally {
    await commands.stop('static');
  }
}

async function record(backend, id, shop, journey) {
  const cwd = await workspace({});
  const registry = await workspace({});
  const env = { FERRIDRIVER_SESSION_DIR: registry, FERRIDRIVER_NO_INHERIT: '1' };
  passed(await run(['session', 'open', id, '--headless', '--backend', backend, shop], { cwd, env }));
  try {
    const command = ['env', `FERRIDRIVER_SESSION_DIR=${registry}`, binary, 'codegen', '--attach', id,
      '--target', 'test', '-o', 'recorded.spec.ts'].map(quote).join(' ');
    await commands.open('stdio', { command: `cd ${quote(cwd)} && exec ${command} 2>&1` });
    // The opening navigation is recorded once the recorder is in every
    // frame; input before that would go unrecorded.
    await readUntil('stdio', 'recorded navigate');
    passed(await run(['run', '--session', id, '--eval', helpers + journey], { cwd, env }));
    await commands.write('stdio', 'stop\n');
    const output = await readUntil('stdio', 'wrote');
    assert.equal(await commands.wait('stdio', 20000), 0, output);
    const spec = await readFile(join(cwd, 'recorded.spec.ts'), 'utf8');
    const lines = spec.split('\n');
    assert.equal(lines[0], "import { test, expect } from '@ferridriver/test';", spec);
    assert.equal(lines[2], "test('test', async ({ page }) => {", spec);
    assert.equal(lines[3], `  await page.goto('${shop}');`, spec);
    const end = lines.indexOf('});');
    assert.notEqual(end, -1, spec);
    return { body: lines.slice(4, end), cwd };
  } finally {
    await commands.stop('stdio');
    await run(['session', 'close', id], { cwd, env });
  }
}

for (const backend of ['cdp-pipe', 'cdp-raw', 'webkit', 'bidi']) {
  test(`${backend}: records a click on a text field and a select its page sets from script`, async () => {
    await journey(backend, `form-${backend}`, `
      await click(page, page.getByRole('textbox', { name: 'Name' }));
      await page.keyboard.type('Sashoush');
      // A remote viewer picks an option by setting the value and
      // dispatching what a pick dispatches.
      await page.evaluate(() => {
        const select = document.querySelector('select');
        select.value = 'Medium';
        select.dispatchEvent(new Event('input', { bubbles: true }));
        select.dispatchEvent(new Event('change', { bubbles: true }));
      });
      await click(page, page.getByRole('button', { name: 'Order' }));
    `, [
      "  await page.getByRole('textbox', { name: 'Name' }).click();",
      "  await page.getByRole('textbox', { name: 'Name' }).fill('Sashoush');",
      "  await page.getByLabel('Size SmallMediumLarge').selectOption('Medium');",
      "  await page.getByRole('button', { name: 'Order' }).click();",
    ]);
  });

  test(`${backend}: records inside an iframe through the frame element`, async () => {
    await journey(backend, `frame-${backend}`, `
      const coupon = page.frameLocator('iframe[title="Coupon"]');
      await click(page, coupon.getByRole('textbox', { name: 'Code' }));
      await page.keyboard.type('SAVE10');
      await click(page, coupon.getByRole('button', { name: 'Apply' }));
    `, [
      "  await page.locator('iframe[title=\"Coupon\"]').contentFrame().getByRole('textbox', { name: 'Code' }).click();",
      "  await page.locator('iframe[title=\"Coupon\"]').contentFrame().getByRole('textbox', { name: 'Code' }).fill('SAVE10');",
      "  await page.locator('iframe[title=\"Coupon\"]').contentFrame().getByRole('button', { name: 'Apply' }).click();",
    ]);
  });

  test(`${backend}: starts a frame path in any frame where that picks out the frame alone`, async () => {
    await journey(backend, `deep-${backend}`, `
      const inner = page.frameLocator('iframe[title="Outer"]').frameLocator('iframe[title="Inner"]');
      await click(page, inner.getByRole('button', { name: 'Deep' }));
      await click(page, inner.getByRole('button', { name: 'Only' }));
    `, [
      // Another frame has a Deep button too, so its path stays whole.
      "  await page.locator('iframe[title=\"Outer\"]').contentFrame().locator('iframe[title=\"Inner\"]').contentFrame().getByRole('button', { name: 'Deep' }).click();",
      "  await page.frameLocator().getByRole('button', { name: 'Only' }).click();",
    ], 'frames');
  });

  test(`${backend}: records the button, held keys, count and canvas point of a click`, async () => {
    await journey(backend, `clicks-${backend}`, `
      const at = async loc => { const [x, y] = await center(loc); await page.mouse.move(x, y); return [x, y]; };
      const button = name => page.getByRole('button', { name });
      let [x, y] = await at(button('Alpha'));
      await page.mouse.click(x, y, { clickCount: 3 });
      [x, y] = await at(button('Bravo'));
      await page.keyboard.down('Shift'); await page.mouse.click(x, y); await page.keyboard.up('Shift');
      [x, y] = await at(button('Charlie'));
      await page.keyboard.down('Alt'); await page.keyboard.down('ControlOrMeta');
      await page.mouse.click(x, y);
      await page.keyboard.up('ControlOrMeta'); await page.keyboard.up('Alt');
      [x, y] = await at(button('Delta'));
      await page.mouse.click(x, y, { button: 'middle' });
      [x, y] = await at(button('Echo'));
      await page.mouse.click(x, y, { button: 'right' });
      [x, y] = await at(button('Foxtrot'));
      // One press after another, as a person double-clicks: Firefox counts
      // the clicks of one input action only.
      await page.keyboard.down('Shift'); await page.mouse.click(x, y, { clickCount: 2 }); await page.keyboard.up('Shift');
      const canvas = await page.locator('canvas').boundingBox();
      await page.mouse.move(canvas.x + 5, canvas.y + 7);
      await page.mouse.click(canvas.x + 5, canvas.y + 7);
    `, [
      "  await page.getByRole('button', { name: 'Alpha' }).click({",
      "    clickCount: 3",
      "  });",
      "  await page.getByRole('button', { name: 'Bravo' }).click({",
      "    modifiers: ['Shift']",
      "  });",
      "  await page.getByRole('button', { name: 'Charlie' }).click({",
      "    modifiers: ['Alt', 'ControlOrMeta']",
      "  });",
      "  await page.getByRole('button', { name: 'Delta' }).click({",
      "    button: 'middle'",
      "  });",
      "  await page.getByRole('button', { name: 'Echo' }).click({",
      "    button: 'right'",
      "  });",
      "  await page.getByRole('button', { name: 'Foxtrot' }).dblclick({",
      "    modifiers: ['Shift']",
      "  });",
      "  await page.locator('canvas').click({",
      "    position: {",
      "      x: 5,",
      "      y: 7",
      "    }",
      "  });",
    ], 'clicks');
  });

  test(`${backend}: records the popup a target=_blank link opens, and the opener after it`, async () => {
    await journey(backend, `popup-${backend}`, `
      const opened = page.waitForEvent('popup');
      await click(page, page.getByRole('link', { name: 'Details' }));
      const details = await opened;
      await details.waitForLoadState();
      await click(details, details.getByRole('textbox', { name: 'Email' }));
      await details.keyboard.type('sashoush@example.com');
      await click(details, details.getByRole('button', { name: 'Subscribe' }));
      await click(page, page.getByRole('heading', { name: 'Acme shop' }));
    `, [
      "  const page1Promise = page.waitForEvent('popup');",
      "  await page.getByRole('link', { name: 'Details' }).click();",
      "  const page1 = await page1Promise;",
      "  await page1.getByRole('textbox', { name: 'Email' }).click();",
      "  await page1.getByRole('textbox', { name: 'Email' }).fill('sashoush@example.com');",
      "  await page1.getByRole('button', { name: 'Subscribe' }).click();",
      "  await page.getByRole('heading', { name: 'Acme shop' }).click();",
    ]);
  });
}
