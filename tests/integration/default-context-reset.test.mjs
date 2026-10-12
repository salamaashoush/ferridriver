import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer, observation, runtimeProbe } from './support.mjs';

const action = (action, options = {}) => ({ action, ...options });
const leftovers = action('evaluate', {
  expression: "({ stored: localStorage.getItem('left'), cookie: document.cookie.includes('left=1'), script: window.leftByScript ?? null })",
});

// The runner's only separation between tests on a browser with one
// context (Android Chrome): what `Browser::reset_default_context` clears.
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`[${backend}] a reset default context holds nothing of its previous user`, async () => {
    await fixtureServer(async base => {
      const url = `${base}/fx/landed`;
      const { results } = await runtimeProbe([{ op: 'browser-lifecycle', backend, actions: [
        action('registries'),
        action('default-context'), action('new-page'),
        action('add-init-script', { source: 'window.leftByScript = 1' }),
        action('navigate', { url }),
        action('evaluate', { expression: "localStorage.setItem('left', '1'); document.cookie = 'left=1'; 1" }),
        leftovers,
        action('new-page'),
        action('context-state'), action('registries'),
        action('reset-default-context', { testIdAttribute: 'data-reset' }),
        action('context-state'), action('registries'),
        action('navigate', { url }),
        leftovers,
        action('context-state'),
      ] }]);
      const seen = observation(results[0]);
      const [baseline, used, usedRegistries, reset, resetRegistries] = [seen[0], seen[8], seen[9], seen[11], seen[12]];

      assert.deepEqual(seen[6], { stored: '1', cookie: true, script: 1 }, 'the first user left all three behind');
      assert.equal(used.pages.length, 2);
      assert.ok(used.cookies.includes('left'), JSON.stringify(used));
      assert.equal(usedRegistries.context_init_scripts, baseline.context_init_scripts + 1);

      assert.deepEqual(reset.pages, ['about:blank']);
      assert.deepEqual(reset.cookies, []);
      assert.equal(reset.testIdAttribute, 'data-reset');
      assert.equal(resetRegistries.context_init_scripts, baseline.context_init_scripts);
      assert.deepEqual(seen[14], { stored: null, cookie: false, script: null });
      assert.deepEqual(seen[15].pages, [url], 'the page handed back is the one the context lists');
    });
  });
}
