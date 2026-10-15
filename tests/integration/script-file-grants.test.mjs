// The browser bindings open files a script names: an upload, an init
// script, a screenshot, a saved state. Each is held to the realm's own
// `read` / `write` grants, as the standard library's `fs` is, so a narrowed
// session cannot read or write around them.
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

test('file paths in browser calls stay inside the read and write grants', async () => {
  const cwd = await workspace({ 'granted/tag.js': 'window.__tag = 1;', 'outside/secret.txt': 'sashoush' });
  const granted = join(cwd, 'granted');
  const outside = join(cwd, 'outside');
  const config = `[scripting.permissions]\nread = [${JSON.stringify(granted)}]\nwrite = [${JSON.stringify(granted)}]\nnet = true\nsys = true\n`;
  const { writeFile } = await import('node:fs/promises');
  await writeFile(join(cwd, 'ferridriver.toml'), config);
  const result = await run(['run', '--no-inherit', '--config', 'ferridriver.toml', '--json', '-e', `
    const browser = await chromium().launch({ headless: true });
    const context = await browser.newContext();
    const page = await context.newPage();
    const out = {};
    const attempt = async (name, work) => {
      try { await work(); out[name] = 'allowed'; } catch (e) { out[name] = String(e.message || e); }
    };
    await page.setContent('<input type="file" id="f">');
    const granted = ${JSON.stringify(granted)};
    const outside = ${JSON.stringify(outside)};
    await attempt('upload', () => page.setInputFiles('#f', outside + '/secret.txt'));
    await attempt('upload granted', () => page.setInputFiles('#f', granted + '/tag.js'));
    await attempt('init script', () => page.addInitScript({ path: outside + '/secret.txt' }));
    await attempt('script tag', () => page.addScriptTag({ path: outside + '/secret.txt' }));
    await attempt('script tag granted', () => page.addScriptTag({ path: granted + '/tag.js' }));
    await attempt('style tag', () => page.mainFrame().addStyleTag({ path: outside + '/secret.txt' }));
    await attempt('screenshot', () => page.screenshot({ path: outside + '/shot.png' }));
    await attempt('screenshot granted', () => page.screenshot({ path: granted + '/shot.png' }));
    await attempt('storage state', () => context.storageState({ path: outside + '/state.json' }));
    await attempt('har replay', () => page.routeFromHAR(outside + '/secret.txt'));
    await attempt('record video', () => browser.newContext({ recordVideo: { dir: outside } }));
    await browser.close();
    return out;
  `], { cwd });
  assert.equal(result.code, 0, result.text);
  const out = JSON.parse(result.stdout).value;
  for (const name of ['upload granted', 'script tag granted', 'screenshot granted']) {
    assert.equal(out[name], 'allowed', name);
  }
  for (const name of ['upload', 'init script', 'script tag', 'style tag', 'har replay']) {
    assert.match(out[name], /permission denied: read access/, name);
  }
  for (const name of ['screenshot', 'storage state', 'record video']) {
    assert.match(out[name], /permission denied: write access/, name);
  }
});
