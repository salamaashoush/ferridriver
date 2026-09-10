import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer, observation, runtimeProbe } from './support.mjs';

async function probe(scenario, files) {
  const { results } = await runtimeProbe([{ op: 'fixture-route', scenario }], files);
  return observation(results[0]);
}

test('fixture TLS rejects its self-signed certificate unless explicitly accepted', async () => {
  assert.deepEqual(await probe('tls'), { rejected: true, status: 200, body: 'secured!!' });
});

test('fixture WebSocket echoes text and binary frames', async () => {
  assert.deepEqual(await probe('websocket'), { text: 'ping-1', binary: [1, 2, 3] });
});

test('fixture proxy traversal records requests and supports clearing its log', async () => {
  const result = await probe('proxy');
  assert.equal(result.advertised, result.actual);
  assert.ok(result.body.includes('PROXY:ok'), result.body);
  assert.ok(result.log.hits >= 1);
  assert.ok(result.log.lines.some(line => line.includes('behind-proxy')));
  assert.equal(result.cleared.hits, 0);
});

test('fixture static fallback serves files while fixture routes take precedence', async () => {
  const result = await probe('static', {
    'hello.html': '<!doctype html><body>static-ok</body>',
    'fx/landed': 'static route must not override the fixture',
  });
  assert.ok(result.static.includes('static-ok'));
  assert.equal(result.landed, 'landed');
});

test('fixture redirect chain exposes each location and reaches its landing page', async ({ request }) => {
  await fixtureServer(async base => {
    for (const [path, location] of [['/fx/redirect', '/fx/landed'], ['/fx/redirect/3', '/fx/redirect/2']]) {
      const response = await request.get(base + path, { maxRedirects: 0 });
      assert.equal(response.status(), 302);
      assert.equal(response.headers().location, location);
    }
    const response = await request.get(base + '/fx/redirect/3');
    assert.equal(response.status(), 200);
    assert.ok(new URL(response.url()).pathname.endsWith('/fx/landed'));
    assert.equal(await response.text(), 'landed');
  });
});

test('fixture API routes echo bodies and request headers', async ({ request }) => {
  await fixtureServer(async base => {
    assert.deepEqual(await (await request.get(base + '/fx/api/users')).json(), { users: ['alice', 'bob'] });
    assert.deepEqual(await (await request.get(base + '/fx/api/posts')).json(), { posts: ['first'] });
    const echoed = await request.post(base + '/fx/echo', { data: 'hello echo' });
    assert.equal(echoed.status(), 200);
    assert.equal(await echoed.text(), 'hello echo');
    const headers = await (await request.get(base + '/fx/echo-headers', { headers: { 'x-fx-marker': '42' } })).json();
    assert.equal(headers['x-fx-marker'], '42');
  });
});

test('fixture HTTP echo reports methods, paths, and JSON payloads', async ({ request }) => {
  await fixtureServer(async base => {
    const echoed = await (await request.post(base + '/_api/post', { data: { name: 'sashoush', role: 'admin' } })).json();
    assert.equal(echoed.url, '/_api/post');
    assert.equal(echoed.method, 'POST');
    assert.equal(echoed.json.name, 'sashoush');
    assert.equal(echoed.json.role, 'admin');
    const deleted = await (await request.delete(base + '/_api/delete')).json();
    assert.equal(deleted.method, 'DELETE');
    assert.equal(deleted.url, '/_api/delete');
  });
});

test('fixture cookie routes retain separate headers and decode cookie values', async ({ request }) => {
  await fixtureServer(async base => {
    for (const [path, expected] of [
      ['/fx/multi-cookie', ['a=1; Path=/', 'b=2; Path=/']],
      ['/fx/set-cookie?c=session%3Dabc%3B%20Path%3D%2F', ['session=abc; Path=/']],
    ]) {
      const response = await request.get(base + path);
      assert.deepEqual(response.headersArray().filter(header => header.name.toLowerCase() === 'set-cookie')
        .map(header => header.value), expected);
    }
  });
});

test('fixture basic authentication challenges missing and incorrect credentials', async ({ request }) => {
  await fixtureServer(async base => {
    const missing = await request.get(base + '/fx/auth');
    assert.equal(missing.status(), 401);
    assert.equal(missing.headers()['www-authenticate'], 'Basic realm="fx"');
    assert.equal(await missing.text(), 'NOAUTH');
    const valid = await request.get(base + '/fx/auth', { headers: { authorization: 'Basic dXNlcjpwYXNz' } });
    assert.equal(valid.status(), 200);
    assert.equal(await valid.text(), 'AUTHED');
    const wrong = await request.get(base + '/fx/auth', { headers: { authorization: 'Basic dXNlcjp3cm9uZw==' } });
    assert.equal(wrong.status(), 401);
  });
});

test('fixture CSP, download, and iframe responses retain their content contracts', async ({ request }) => {
  await fixtureServer(async base => {
    const csp = await request.get(base + '/fx/csp');
    assert.equal(csp.headers()['content-security-policy'], "script-src 'none'");
    const download = await request.get(base + '/fx/download');
    assert.equal(download.headers()['content-disposition'], 'attachment; filename="greeting.txt"');
    assert.deepEqual([...await download.body()], [...new TextEncoder().encode('fx-download-payload')]);
    assert.ok((await (await request.get(base + '/fx/iframe')).text()).includes('<iframe src="/fx/inner">'));
    assert.ok((await (await request.get(base + '/fx/inner')).text()).includes('inner'));
  });
});
