import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, repo, runtimeProbe } from './support.mjs';

async function transport(actions) {
  const { results } = await runtimeProbe([{ op: 'sidecar-transport', binary: join(repo, 'target/debug/sidecar_echo'), actions }]);
  return observation(results[0]);
}
const send = (method, params) => ({ action: 'send', call: { method, params } });

test('sidecar transport ping round-trips through fd 3 and 4', async () => {
  const [result] = await transport([send('ping')]);
  assert.equal(observation(result).ok, true);
});

test('sidecar transport preserves structured parameters', async () => {
  const [result] = await transport([send('echo', { a: 1, b: 'x' })]);
  assert.deepEqual(observation(result), { a: 1, b: 'x' });
});

test('sidecar transport remains usable after a remote error', async () => {
  const [failed, ping] = await transport([send('__nope__'), send('ping')]);
  assert.ok(failed.error.includes('unknown method'));
  assert.equal(observation(ping).ok, true);
});

test('sidecar transport correlates concurrent responses by request ID', async () => {
  const [results] = await transport([{ action: 'concurrent', calls: [1, 2, 3].map(params => ({ method: 'echo', params })) }]);
  assert.deepEqual(results.map(observation), [1, 2, 3]);
});

test('sidecar transport batch results retain all fifty input positions', async () => {
  const expected = Array.from({ length: 50 }, (_, i) => i);
  const [results] = await transport([{ action: 'batch', calls: expected.map(params => ({ method: 'echo', params })) }]);
  assert.equal(results.length, 50);
  assert.deepEqual(results.map(observation), expected);
});

test('sidecar transport batch errors do not discard successful neighboring calls', async () => {
  const [results] = await transport([{ action: 'batch', calls: [
    { method: 'ping' }, { method: '__nope__' }, { method: 'echo', params: 'z' },
  ] }]);
  assert.equal(results.length, 3);
  assert.equal(observation(results[0]).ok, true);
  assert.ok(results[1].error.includes('unknown method'));
  assert.equal(observation(results[2]), 'z');
});

test('sidecar transport empty batches leave the connection usable', async () => {
  const [batch, ping] = await transport([{ action: 'batch', calls: [] }, send('ping')]);
  assert.deepEqual(batch, []);
  assert.equal(observation(ping).ok, true);
});

test('sidecar transport delivers pushed events to existing subscribers', async () => {
  const [ack, event] = await transport([send('emit', { event: 'tick', payload: { n: 42 } }), { action: 'event' }]);
  assert.equal(observation(ack).ok, true);
  assert.deepEqual(event, ['tick', { n: 42 }]);
});

test('sidecar transport signals child death and permits repeated close waits', async () => {
  const [before, exited, repeated, closed] = await transport([
    { action: 'closed' }, { action: 'exit-with-waiters' }, { action: 'wait-closed' }, { action: 'close' },
  ]);
  assert.equal(before, false);
  assert.equal(exited.closed, true);
  assert.equal(repeated, true);
  assert.equal(closed, null);
});

test('sidecar transport explicit close remains observable by later waiters', async () => {
  const [before, closed, after] = await transport([{ action: 'closed' }, { action: 'close' }, { action: 'wait-closed' }]);
  assert.equal(before, false);
  assert.equal(closed, null);
  assert.equal(after, true);
});
