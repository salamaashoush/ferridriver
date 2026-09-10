import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer, observation, runtimeProbe } from './support.mjs';

async function probe(request) {
  const { results } = await runtimeProbe([{ op: 'webkit', request }]);
  return observation(results[0]);
}

test('WebKit backend navigates, evaluates, and closes its page and browser', async () => {
  const result = await probe({ scenario: 'navigation' });
  assert.equal(result.sum, 2);
  assert.ok(result.html.includes('hello'), result.html);
});

test('WebKit locale changes reach cross-site navigation and newly created pages', async () => {
  await fixtureServer(async base => {
    const result = await probe({ scenario: 'locale', url: base });
    assert.equal(result.before, 'en-US');
    assert.equal(result.after, 'de-DE');
    assert.equal(result.fresh, 'de-DE');
  });
});

test('WebKit mobile layout and feature detection match the configured device', async () => {
  const result = await probe({ scenario: 'mobile' });
  assert.deepEqual(JSON.parse(result.probe), {
    gesture: 'function', safari: 'object', pkc: 'object', width: 390, dpr: 3, touch: true,
  });
});

test('WebKit desktop pages do not expose mobile orientation', async () => {
  assert.equal((await probe({ scenario: 'desktop' })).orientation, false);
});
