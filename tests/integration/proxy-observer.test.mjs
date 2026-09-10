import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer } from './support.mjs';

test('resetting one proxy observer preserves another concurrent observer', async ({ request }) => {
  await fixtureServer(async base => {
    const info = await (await request.get(`${base}/fx/proxy-info`)).json();
    await Promise.all(['scope-a', 'scope-ab'].map(key => request.get(`${info.url}/probe?key=${key}`)));
    const before = await (await request.get(`${base}/fx/proxy-log`)).json();
    assert.equal(before.lines.length, 2);
    await request.delete(`${base}/fx/proxy-log?key=scope-a`);
    const after = await (await request.get(`${base}/fx/proxy-log`)).json();
    assert.equal(after.hits, 1);
    assert.ok(after.lines[0].includes('key=scope-ab'));
    await request.delete(`${base}/fx/proxy-log?key=scope-ab`);
    const empty = await (await request.get(`${base}/fx/proxy-log`)).json();
    assert.deepEqual(empty, { hits: 0, lines: [] });
  });
});
