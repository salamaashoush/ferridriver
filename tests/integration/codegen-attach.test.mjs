import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { binary, passed, quote, run, workspace } from './support.mjs';

async function readUntil(name, text) {
  let seen = '';
  while (!seen.includes(text)) {
    const chunk = await commands.read(name, 20000);
    assert.notEqual(chunk, null, `exited before printing ${text}:\n${seen}`);
    seen += chunk + '\n';
  }
  return seen;
}

for (const backend of ['cdp-pipe', 'webkit', 'bidi']) {
  test(`codegen --attach records a headless ${backend} session page into a spec that passes`, async () => {
    const cwd = await workspace({});
    const registry = await workspace({});
    const env = { FERRIDRIVER_SESSION_DIR: registry, FERRIDRIVER_NO_INHERIT: '1' };
    const id = `rec-${backend}`;
    const page = 'data:text/html,<title>Profile</title><label>Name <input id=name></label>' +
      '<button onclick="document.title=\'Saved\'">Save</button>';
    passed(await run(['session', 'open', id, '--headless', '--backend', backend, page], { cwd, env }));
    try {
      const command = ['env', `FERRIDRIVER_SESSION_DIR=${registry}`, binary, 'codegen', '--attach', id,
        '--target', 'test', '--title', 'saves a profile', '-o', 'saved.spec.ts'].map(quote).join(' ');
      await commands.open('stdio', { command: `cd ${quote(cwd)} && exec ${command} 2>&1` });
      await readUntil('stdio', 'type stop');

      // Input from a script is real browser input, so the recorder sees it as
      // it would a person's.
      passed(await run(['run', '--session', id, '--eval',
        "await page.locator('#name').click(); await page.keyboard.type('sashoush'); await page.getByRole('button').click();"],
      { cwd, env }));
      await commands.write('stdio', 'title\n');
      await commands.write('stdio', 'stop\n');
      const output = await readUntil('stdio', 'wrote');
      assert.equal(await commands.wait('stdio', 20000), 0, output);

      const spec = await readFile(join(cwd, 'saved.spec.ts'), 'utf8');
      assert.ok(spec.startsWith("import { test, expect } from '@ferridriver/test';\n\ntest('saves a profile', async ({ page }) => {\n"), spec);
      assert.ok(spec.includes("  await page.getByRole('textbox', { name: 'Name' }).fill('sashoush');\n"), spec);
      assert.ok(spec.includes("  await page.getByRole('button', { name: 'Save' }).click();\n"), spec);
      assert.ok(spec.includes("  await expect(page).toHaveTitle('Saved');\n"), spec);
      passed(await run(['test', '--no-inherit', '--headless', 'saved.spec.ts'], { cwd }));
    } finally {
      await commands.stop('stdio');
      await run(['session', 'close', id], { cwd, env });
    }
  });
}
