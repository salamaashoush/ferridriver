import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer } from './support.mjs';

test('requests without a redirect cap follow the complete chain', async ({ request }) => {
  await fixtureServer(async base => {
    const response = await request.get(`${base}/fx/http-client/redirect/3`);
    assert.equal(response.status(), 200);
    assert.equal(await response.text(), 'done');
  });
});

test('a zero redirect cap returns the original redirect response', async ({ request }) => {
  await fixtureServer(async base => {
    const response = await request.get(`${base}/fx/http-client/redirect/3`, { maxRedirects: 0 });
    assert.equal(response.status(), 302);
    assert.notEqual(await response.text(), 'done');
  });
});

test('redirect limits apply per request without changing the client', async ({ request }) => {
  await fixtureServer(async base => {
    const url = `${base}/fx/http-client/redirect/3`;
    await assert.rejects(() => request.get(url, { maxRedirects: 2 }));
    const response = await request.get(url, { maxRedirects: 5 });
    assert.equal(response.status(), 200);
    assert.equal(await response.text(), 'done');
  });
});

test('manual redirects report an unfollowed response', async ({ request }) => {
  await fixtureServer(async base => {
    const response = await request.get(`${base}/fx/http-client/redirect/3`, { redirect: 'manual' });
    assert.equal(response.status(), 302);
    assert.equal(response.unfollowedRedirect(), true);
    assert.equal(response.redirected(), false);
  });
});

test('error redirect mode rejects a redirect response', async ({ request }) => {
  await fixtureServer(async base => {
    await assert.rejects(() => request.get(`${base}/fx/http-client/redirect/1`, { redirect: 'error' }));
  });
});

test('redirect metadata counts followed hops rather than appended query parameters', async ({ request }) => {
  await fixtureServer(async base => {
    const response = await request.get(`${base}/fx/http-client/redirect/2`);
    assert.equal(response.status(), 200);
    assert.equal(await response.text(), 'done');
    assert.equal(response.redirected(), true);
    const plain = await request.get(`${base}/fx/http-client/echo`, { params: { a: '1' } });
    assert.equal(plain.redirected(), false);
  });
});

test('multipart requests preserve text fields file headers bytes and boundary content type', async ({ request }) => {
  await fixtureServer(async base => {
    const response = await request.post(`${base}/fx/http-client/body-echo`, { multipart: {
      field: 'hello', upload: { name: 'a.txt', mimeType: 'text/plain', buffer: 'file-bytes' },
    } });
    const echoed = await response.text();
    assert.ok(echoed.includes('Content-Disposition: form-data; name="field"'), echoed);
    assert.ok(echoed.includes('hello'), echoed);
    assert.ok(echoed.includes('filename="a.txt"') && echoed.includes('Content-Type: text/plain'), echoed);
    assert.ok(echoed.includes('file-bytes'), echoed);
    const headers = await request.post(`${base}/fx/http-client/ct-echo`, { multipart: { x: 'y' } });
    assert.ok((await headers.text()).startsWith('multipart/form-data; boundary='));
  });
});

test('the standalone cookie jar persists across requests with different redirect limits', async ({ request }) => {
  await fixtureServer(async base => {
    const response = await request.get(`${base}/fx/http-client/set`, { maxRedirects: 0 });
    assert.equal(await response.text(), 'set');
    const echoed = await request.get(`${base}/fx/http-client/echo`);
    assert.equal(await echoed.text(), 'sid=abc');
  });
});
