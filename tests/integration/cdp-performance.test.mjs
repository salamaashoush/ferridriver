import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

test('native page scripting exposes a lossless Chrome performance trace', async () => {
  const { results } = await runtimeProbe([{
    op: 'browser-engine',
    scripts: [{
      source: `
        await page.startTracing(['devtools.timeline', 'v8.execute']);
        await page.goto('data:text/html,<title>trace</title><button id="go">go</button>');
        await page.locator('#go').click();
        const metrics = await page.metrics();
        const events = await page.stopTracing();
        return {
          count: events.length,
          metricNames: metrics.map(metric => metric.name),
          names: [...new Set(events.map(event => event.name).filter(Boolean))].slice(0, 80),
          hasNavigation: events.some(event => event.name === 'CommitLoad' || event.name === 'MarkLoad'),
          hasScript: events.some(event => event.name === 'FunctionCall' || event.name === 'RunMicrotasks'),
        };
      `,
    }],
  }]);
  const result = observation(results[0])[0];
  assert.equal(result.status, 'ok', JSON.stringify(result));
  assert.ok(result.value.count > 0, JSON.stringify(result.value));
  assert.ok(result.value.metricNames.includes('Timestamp'), JSON.stringify(result.value));
  assert.equal(result.value.hasNavigation, true, JSON.stringify(result.value));
  assert.equal(result.value.hasScript, true, JSON.stringify(result.value));
});

test('native tracing reports an explicit error when stopped before it starts', async () => {
  const { results } = await runtimeProbe([{
    op: 'browser-engine',
    scripts: [{ source: 'return await page.stopTracing();' }],
  }]);
  const result = observation(results[0])[0];
  assert.equal(result.status, 'error', JSON.stringify(result));
  assert.match(result.error.message, /Tracing is not started|Tracing has not been started|Must start tracing/);
});
