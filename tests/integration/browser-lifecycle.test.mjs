import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const action = (action, options = {}) => ({ action, ...options });
const open = [action('new-context', { userAgent: 'ferri-teardown-probe' }),
  action('new-page'), action('set-content', { html: '<h1>probe</h1>' })];

async function lifecycle(actions) {
  const { results } = await runtimeProbe([{ op: 'browser-lifecycle', actions }]);
  return observation(results[0]);
}

test('closing five contexts restores every per-context registry', async () => {
  const cycle = [...open, action('registries'), action('close-context'), action('registries')];
  const results = await lifecycle([action('registries'), ...Array.from({ length: 5 }, () => cycle).flat()]);
  const before = results[0];
  assert.deepEqual(Object.keys(before).sort(), [
    'context_options', 'context_events', 'context_closed', 'record_video', 'har_recorders',
    'context_har_updates', 'clock_installed', 'storage_state_hydrated', 'context_bindings',
    'context_ws_routes', 'context_routes', 'context_init_scripts',
  ].sort());
  for (let cycle = 0; cycle < 5; cycle++) {
    const populated = results[1 + cycle * 6 + 3];
    assert.ok(populated.context_options > before.context_options, `cycle ${cycle}: options were registered`);
    assert.deepEqual(results[1 + cycle * 6 + 5], before, `cycle ${cycle}: context state leaked`);
  }
});

test('a browser-closed target is marked closed and cannot remain active', async () => {
  const results = await lifecycle([...open, action('page-state'), action('self-close'), action('page-state')]);
  assert.equal(results[3].closed, false, 'the original page starts live');
  assert.equal(results[5].closed, true, 'the browser close event marks the page closed');
  assert.notEqual(results[5].activeClosed, true, 'a closed target cannot remain the active page');
});

test('a context accepts a usable replacement after its only target closes itself', async () => {
  const results = await lifecycle([...open, action('self-close'), action('page-state'),
    action('new-page'), action('page-state'), action('evaluate', { expression: '1 + 1' })]);
  assert.equal(results[4].closed, true, 'the original target is gone before recovery');
  assert.equal(results[6].closed, false, 'the replacement target is live');
  assert.equal(results[7], 2, 'the replacement serves browser commands');
});
