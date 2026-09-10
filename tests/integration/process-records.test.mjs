import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

test('concurrent launch-record readers only observe complete JSON', async () => {
  const { results } = await runtimeProbe([{ op: 'process-record-publication' }]);
  const result = observation(results[0]);
  assert.equal(result.alive, true, JSON.stringify(result));
  assert.equal(result.writes, 64);
  assert.ok(result.reads > 0, JSON.stringify(result));
  assert.equal(result.ioErrors, 0, JSON.stringify(result));
  assert.equal(result.malformed, 0, JSON.stringify(result));
});
