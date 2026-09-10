import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

for (const signal of ['SIGTERM', 'SIGINT', null]) {
  test(`web server shutdown ${signal ?? 'SIGKILL'} finishes before returning`, async () => {
    const { results } = await runtimeProbe([{ op: 'web-server-shutdown', signal }]);
    const result = observation(results[0]);
    assert.equal(result.firstUrl, result.url);
    assert.equal(result.status, 200);
    assert.equal(result.body, 'ok');
    assert.equal(result.marker, signal);
    assert.equal(result.reachableAfterStop, false);
  });
}

test('web server readiness accepts HTTP and applies TLS certificate policy to HTTPS', async () => {
  const { results } = await runtimeProbe([{ op: 'web-server-probes' }]);
  assert.deepEqual(observation(results[0]), {
    httpStrict: true, httpLenient: true, httpsStrict: false, httpsLenient: true,
  });
});

test('web server startup drains stdout and stderr before their pipes fill', async () => {
  const { results } = await runtimeProbe([{ op: 'web-server-shutdown', signal: 'SIGTERM', noisy: true }]);
  const result = observation(results[0]);
  assert.equal(result.firstUrl, result.url);
  assert.equal(result.status, 200);
  assert.equal(result.body, 'ok');
  assert.equal(result.marker, 'SIGTERM');
  assert.equal(result.reachableAfterStop, false);
});
