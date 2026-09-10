import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

test('BDD step modules see no environment variables unless capabilities are configured', async () => {
  const { results } = await runtimeProbe([{ op: 'load-bdd', globs: ['steps/**/*.js'] }], {
    'steps/cap.js': `if (Object.keys(process.env).length !== 0) throw new Error('env leaked: ' + JSON.stringify(process.env));
      Given('a no-op', function () {});`,
  }, { FERRIDRIVER_BDD_CAPTEST_OFF: 'leak' });
  assert.ok(observation(results[0]).includes('a no-op'));
});

test('BDD step modules receive allow-listed environment variables during module evaluation', async () => {
  const { results } = await runtimeProbe([{ op: 'load-bdd', globs: ['steps/**/*.js'], allowEnv: ['FERRIDRIVER_BDD_CAPTEST'] }], {
    'steps/cap.js': `if (process.env.FERRIDRIVER_BDD_CAPTEST !== 'yes') throw new Error('env cap missing: ' + JSON.stringify(process.env));
      Given('a no-op', function () {});`,
  }, { FERRIDRIVER_BDD_CAPTEST: 'yes' });
  assert.ok(observation(results[0]).includes('a no-op'));
});
