import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

for (const scenario of ['chromium', 'firefox', 'webkit']) {
  test(`persistent profile override is used and retained by ${scenario}`, async () => {
    const { results } = await runtimeProbe([{ op: 'persistent-profile', scenario }]);
    const result = observation(results[0]);
    assert.deepEqual(result.before, { isDirectory: true, hasEntries: true });
    assert.deepEqual(result.after, { isDirectory: true, hasEntries: true });
  });
}

test('temporary Firefox profile supports launch and shutdown', async () => {
  const { results } = await runtimeProbe([{ op: 'persistent-profile', scenario: 'temporary' }]);
  assert.deepEqual(observation(results[0]).size, [1280, 720]);
});

test('saved window dimensions do not replace the default viewport', async () => {
  const preferences = { browser: { window_placement: {
    left: 0, top: 0, right: 1001, bottom: 777, maximized: false,
  } } };
  const { results } = await runtimeProbe([{ op: 'persistent-profile', scenario: 'saved-size' }], {
    'persistent-profile/Default/Preferences': JSON.stringify(preferences),
  });
  assert.deepEqual(observation(results[0]).size, [1280, 720]);
});

test('adopting an existing tab applies the configured default viewport', async () => {
  const { results } = await runtimeProbe([{ op: 'persistent-profile', scenario: 'adopted' }]);
  assert.deepEqual(observation(results[0]).size, [1280, 720]);
});

test('resizing a maximized window normalizes it before applying the viewport', async () => {
  const { results } = await runtimeProbe([{ op: 'persistent-profile', scenario: 'maximized' }]);
  const result = observation(results[0]);
  assert.equal(result.windowBeforeResize.bounds.windowState, 'maximized');
  assert.deepEqual(result.size, [900, 600]);
});
