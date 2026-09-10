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

for (const failure of ['stall', 'unavailable', 'later-invalid', 'later-spawn', 'early-exit']) {
  test(`web server startup cleans up after ${failure}`, async () => {
    const { results } = await runtimeProbe([{ op: 'web-server-shutdown', signal: 'SIGTERM', failure }]);
    const result = observation(results[0]);
    assert.equal(typeof result.error, 'string', JSON.stringify(result));
    assert.equal(result.reachableAfterFailure, false, JSON.stringify(result));
    if (failure === 'early-exit') {
      assert.match(result.error, /exited.*7/);
      assert.equal(result.marker, null);
    } else {
      assert.equal(result.marker, 'SIGTERM', JSON.stringify(result));
    }
    if (failure === 'stall' || failure === 'unavailable') {
      assert.match(result.error, /Timeout 400ms/);
      assert.ok(result.elapsedMs < 2000, JSON.stringify(result));
    }
  });
}

test('web server reuse keeps the external HTTPS server alive without spawning any command', async () => {
  const { results } = await runtimeProbe([{ op: 'web-server-shutdown', failure: 'reuse' }], {}, { PATH: '' });
  const result = observation(results[0]);
  assert.equal(result.firstUrl, result.url);
  assert.equal(result.reachableAfterStop, true);
});
