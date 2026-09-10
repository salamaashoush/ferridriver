import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

test('CDP reconnect preserves selectable pages and their titles', async () => {
  const url = 'data:text/html,<title>Page2</title><body>Hello2</body>';
  const { results } = await runtimeProbe([{ op: 'cdp-connection', urls: [url] }]);
  const result = observation(results[0]);
  assert.equal(result.runningAfterDisconnect, true);
  const expected = [{ url: 'about:blank', title: '' }, { url, title: 'Page2' }];
  const ordered = pages => [...pages].sort((a, b) => a.url.localeCompare(b.url));
  assert.deepEqual(ordered(result.before), expected);
  assert.deepEqual(ordered(result.after), expected);
});
