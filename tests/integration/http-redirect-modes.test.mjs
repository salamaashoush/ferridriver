import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { repo } from './support.mjs';

async function server(body) {
  await commands.start('fixtures', { binary: join(repo, 'target/debug/ferridriver-fixtures') });
  try {
    const ready = await commands.waitForOutput('fixtures', '\n');
    const match = ready.match(/serving (http:\/\/127\.0\.0\.1:\d+)/);
    assert.ok(match, ready);
    await body(match[1]);
  } finally { await commands.stop('fixtures'); }
}

test('manual request redirects return the original response and report no followed hop', async ({ request }) => {
  await server(async base => {
    const response = await request.get(`${base}/fx/redirect/3`, { redirect: 'manual' });
    assert.equal(response.status(), 302);
    assert.equal(response.unfollowedRedirect(), true);
    assert.equal(response.redirected(), false);
  });
});

test('error redirect mode rejects a redirect instead of silently following it', async ({ request }) => {
  await server(async base => {
    await assert.rejects(() => request.get(`${base}/fx/redirect`, { redirect: 'error' }));
  });
});

test('followed redirects are distinguished from URLs changed only by query parameters', async ({ request }) => {
  await server(async base => {
    const followed = await request.get(`${base}/fx/redirect/2`);
    assert.equal(followed.status(), 200);
    assert.equal(followed.redirected(), true);
    assert.equal(followed.unfollowedRedirect(), false);
    const plain = await request.get(`${base}/fx/landed`, { params: { a: '1' } });
    assert.equal(plain.redirected(), false);
  });
});

test('redirect modes are per-request and invalid modes are rejected', async ({ request }) => {
  await server(async base => {
    const input = `${base}/fx/redirect`;
    const inherited = await request.fetch(input, { redirect: 'manual', headers: { 'x-probe': 'redirect' } });
    assert.equal(inherited.status(), 302);
    assert.equal(inherited.unfollowedRedirect(), true);
    const overridden = await request.fetch(input, { redirect: 'follow' });
    assert.equal(overridden.status(), 200);
    assert.equal(overridden.redirected(), true);
    await assert.rejects(() => request.get(`${base}/fx/redirect`, { redirect: 'invalid' }));
  });
});
