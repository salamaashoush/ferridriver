import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

async function retry(options) {
  const { results } = await runtimeProbe([{
    op: 'execute-virtual-script', source: `
      const started = Date.now();
      let attempts = 0;
      let message = '';
      try {
        await expect(async () => {
          attempts += 1;
          throw new Error('always fails');
        }).toPass(${JSON.stringify(options)});
      } catch (error) { message = String(error.message); }
      return { attempts, message, elapsed: Date.now() - started };
    `,
  }]);
  const result = observation(results[0]);
  assert.equal(result.result.status, 'ok', JSON.stringify(result));
  return { ...result.result.value, virtualElapsed: result.elapsedMs };
}

test('custom retry intervals make at least five attempts within a controlled 400ms deadline', async () => {
  const result = await retry({ intervals: [50], timeout: 400 });
  assert.ok(result.message.includes('always fails'), result.message);
  assert.ok(result.attempts >= 5, JSON.stringify(result));
  assert.ok(result.elapsed < 3000, JSON.stringify(result));
  assert.ok(result.virtualElapsed >= 400 && result.virtualElapsed < 450, JSON.stringify(result));
});

test('the default retry schedule is distinguishable from a custom 50ms interval', async () => {
  const custom = await retry({ intervals: [50], timeout: 400 });
  const defaults = await retry({ timeout: 400 });
  assert.ok(defaults.message.includes('always fails'), defaults.message);
  assert.ok(defaults.attempts < 5, JSON.stringify(defaults));
  assert.ok(custom.attempts > defaults.attempts, JSON.stringify({ custom, defaults }));
});
