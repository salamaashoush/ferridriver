import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer, observation, runtimeProbe } from './support.mjs';

const metadataGuard = { blockMetadata: true, blockPrivate: false };
const loopbackGuard = { ...metadataGuard, allowlist: ['127.0.0.1'] };

async function get(url, guard) {
  const { results } = await runtimeProbe([{ op: 'http-request', url, guard }]);
  return results[0];
}

test('an allowlisted loopback host remains reachable with metadata protection enabled', async () => {
  await fixtureServer(async base => {
    const result = observation(await get(`${base}/fx/http-client/landed`, loopbackGuard));
    assert.equal(result.body, 'LANDED');
  });
});

test('a redirect to localhost cannot bypass a numeric loopback host allowlist', async () => {
  await fixtureServer(async base => {
    const result = await get(`${base}/fx/http-client/hop-offhost`, loopbackGuard);
    assert.equal(typeof result.error, 'string', JSON.stringify(result));
    assert.ok(!result.error.includes('LANDED'), result.error);
  });
});

test('metadata redirects are rejected even when no host allowlist is configured', async () => {
  await fixtureServer(async base => {
    const result = await get(`${base}/fx/http-client/hop-metadata`, metadataGuard);
    assert.equal(typeof result.error, 'string', JSON.stringify(result));
    assert.ok(result.error.includes('blocked address'), result.error);
  });
});

test('a direct metadata request is rejected by address preflight', async () => {
  const result = await get('http://169.254.169.254/latest/meta-data/', metadataGuard);
  assert.equal(typeof result.error, 'string', JSON.stringify(result));
  assert.ok(result.error.includes('blocked address'), result.error);
});

test('the unguarded core request path continues to follow loopback redirects', async () => {
  await fixtureServer(async base => {
    const result = observation(await get(`${base}/fx/http-client/hop-offhost`));
    assert.equal(result.body, 'LANDED');
  });
});
