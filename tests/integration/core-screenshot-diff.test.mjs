import assert from 'node:assert/strict';
import { stat } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, passed, repo, run, workspace } from './support.mjs';

async function setup(color, name) {
  const cwd = await workspace({ 'index.html': `<!DOCTYPE html><html><body style="margin:0;padding:20px;background:white">
    <div id="box" style="width:100px;height:100px;background:${color}"></div></body></html>` });
  const snapshots = join(cwd, '__snapshots__');
  async function capture(update, expression) {
    const result = await run([], {
      cwd, executable: join(repo, 'target/debug/ferridriver-runtime-probe'),
      input: JSON.stringify([{ op: 'screenshot-snapshot', name, expression }]),
      env: { SNAPSHOT_DIR: snapshots, ...(update ? { UPDATE_SNAPSHOTS: '1' } : {}) },
    });
    passed(result);
    return observation(JSON.parse(result.stdout)[0]);
  }
  assert.equal((await capture(true)).matched, true);
  return { capture, snapshots };
}

test('core screenshot matcher creates a real baseline and matches identical content', async () => {
  const { capture, snapshots } = await setup('red', 'red_box');
  assert.ok((await stat(join(snapshots, 'red_box.png'))).size > 100);
  assert.equal((await capture(false)).matched, true);
});

test('core screenshot matcher reports changed pixels and writes actual and diff images', async () => {
  const { capture, snapshots } = await setup('red', 'color_box');
  const result = await capture(false, "() => { document.getElementById('box').style.background = 'blue'; }");
  assert.equal(result.matched, false);
  assert.ok(result.message.includes('mismatch'), result.message);
  assert.ok(result.message.includes('pixels differ'), result.message);
  assert.equal(result.hasScreenshot, true);
  assert.ok((await stat(join(snapshots, 'color_box-diff.png'))).size > 100);
  assert.ok((await stat(join(snapshots, 'color_box-actual.png'))).isFile());
});

test('core screenshot matcher detects changed element dimensions', async () => {
  const { capture } = await setup('green', 'size_box');
  const result = await capture(false,
    "() => { const b = document.getElementById('box'); b.style.width = '200px'; b.style.height = '200px'; }");
  assert.equal(result.matched, false);
  assert.ok(result.message.includes('size mismatch'), result.message);
});
