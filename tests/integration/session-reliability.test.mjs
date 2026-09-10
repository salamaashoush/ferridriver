import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';
import { dataUrl } from './mcp-client.mjs';

function value(result) {
  assert.equal(result.status, 'ok', JSON.stringify(result));
  return result.value;
}
async function sessionTable(request) {
  const { results } = await runtimeProbe([{ op: 'session-table', request }]);
  return observation(results[0]);
}
const call = (name, source, options = {}) => ({ action: 'run', name, source, ...options });

test('session reliability retains live page state through churn and rebuilds only when required', async () => {
  const execute = (source, options = {}) => call('mcp-1', source, { epoch: 1, ...options });
  const results = await sessionTable({
    maxVms: 64, ttlMs: 1800000, browser: true,
    actions: [
      execute(`await page.goto(${JSON.stringify(dataUrl('<title>Long</title><body><h1 id="h">hi</h1></body>'))});
        globalThis.c = 0; vars.set('persisted', 'yes'); return true;`, { page: true }),
      ...Array.from({ length: 40 }, () => execute(`globalThis.c += 1;
        const t = await page.locator('#h').textContent();
        vars.set('lastIter', String(globalThis.c)); return { c: globalThis.c, t };`, { page: true })),
      execute("return { c: globalThis.c, persisted: vars.get('persisted'), last: vars.get('lastIter') };"),
      ...Array.from({ length: 250 }, () => execute(`const a = Array.from({length: 5000}, (_, i) => ({ i, s: 'x'.repeat(32), nested: [i, i*2] }));
        globalThis.churn = (globalThis.churn || 0) + 1; return a.length + globalThis.churn;`)),
      execute('return globalThis.churn;'),
      execute('while (true) {}', { timeoutMs: 200 }),
      execute("globalThis.afterEpoch = 41; return { rebuilt: globalThis.c === undefined, persisted: vars.get('persisted') };"),
      execute(`globalThis.afterEpoch = (globalThis.afterEpoch || 0) + 1;
        return { fresh: globalThis.afterEpoch === 1, persisted: vars.get('persisted') };`, { epoch: 2 }),
    ],
  });
  assert.equal(value(results[0]), true);
  for (let i = 1; i <= 40; i++) assert.deepEqual(value(results[i]), { c: i, t: 'hi' });
  assert.deepEqual(value(results[41]), { c: 40, persisted: 'yes', last: '40' });
  for (let i = 0; i < 250; i++) assert.equal(value(results[42 + i]), 5001 + i);
  assert.equal(value(results[292]), 250);
  assert.equal(results[293].status, 'error');
  assert.deepEqual(value(results[294]), { rebuilt: true, persisted: 'yes' });
  assert.deepEqual(value(results[295]), { fresh: true, persisted: 'yes' });
});

test('session reliability enforces VM capacity and reaps idle durable records', async () => {
  const results = await sessionTable({
    maxVms: 1, ttlMs: 150,
    actions: [
      call('a', "globalThis.tag = 'A'; vars.set('owner', 'a'); return true;"),
      { action: 'live-vms' },
      call('b', "globalThis.tag = 'B'; return true;"),
      { action: 'live-vms' },
      call('a', "return { vmGone: globalThis.tag === undefined, owner: vars.get('owner') };"),
      { action: 'idle', ms: 250 },
      call('c', 'return 1;'),
      call('a', "return vars.get('owner');"),
    ],
  });
  assert.equal(value(results[0]), true);
  assert.equal(results[1], 1);
  assert.equal(value(results[2]), true);
  assert.ok(results[3] <= 1);
  assert.deepEqual(value(results[4]), { vmGone: true, owner: 'a' });
  assert.equal(value(results[6]), 1);
  assert.equal(value(results[7]), null);
});
