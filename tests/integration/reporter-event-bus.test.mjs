import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const started = (totalTests, numWorkers) => ({ kind: 'RunStarted', totalTests, numWorkers });
const finished = (total, passed, failed, skipped, durationMs) =>
  ({ kind: 'RunFinished', total, passed, failed, skipped, durationMs });
const worker = (kind, workerId = 0) => ({ kind, workerId });
const emit = (event, sender = 0) => ({ kind: 'emit', sender, event });
const dropSender = (sender = 0) => ({ kind: 'dropSender', sender });
const receive = (subscriber = 0, immediate = false) => ({ kind: 'receive', subscriber, immediate });

async function drive(subscribers, ...actions) {
  const { results } = await runtimeProbe([{ op: 'reporter-bus', subscribers, actions }]);
  return observation(results[0]);
}

async function report(...events) {
  const { results } = await runtimeProbe([{ op: 'reporter-driver', events }]);
  return observation(results[0]);
}

test('a subscriber receives run metadata and closes after the last bus sender drops', async () => {
  const event = started(5, 2);
  const results = await drive(1, emit(event), dropSender(), receive(), receive());
  assert.deepEqual(results.slice(2), [event, null]);
});

test('every subscriber receives the complete ordered run and channel closure', async () => {
  const begin = started(10, 4);
  const end = finished(10, 8, 1, 1, 5000);
  const reads = [0, 1, 2].flatMap(subscriber => [receive(subscriber), receive(subscriber), receive(subscriber)]);
  const results = await drive(3, emit(begin), emit(end), dropSender(), ...reads);
  assert.deepEqual(results.slice(3), [begin, end, null, begin, end, null, begin, end, null]);
});

test('a worker clone shares subscribers without closing the original sender', async () => {
  const begin = worker('WorkerStarted');
  const end = worker('WorkerFinished');
  const results = await drive(1, { kind: 'clone', sender: 0 }, emit(begin, 1), dropSender(1),
    emit(end), dropSender(), receive(), receive(), receive());
  assert.deepEqual(results.slice(5), [begin, end, null]);
});

test('emitting on a bus with no subscribers completes normally', async () => {
  assert.deepEqual(await drive(0, emit(started(1, 1)), dropSender()), [null, null]);
});

test('dropping one subscriber does not interrupt delivery to the remaining subscriber', async () => {
  const event = started(1, 1);
  const results = await drive(2, { kind: 'dropSubscriber', subscriber: 0 }, emit(event), dropSender(), receive(1));
  assert.deepEqual(results[3], event);
});

test('the reporter driver forwards events in order and finalizes after draining them', async () => {
  const result = await report(started(2, 1), finished(2, 2, 0, 0, 100));
  assert.deepEqual(result.events.map(event => event.kind), ['RunStarted', 'RunFinished', 'Finalized']);
});

test('the reporter driver returns its reporter set after finalization', async () => {
  const result = await report(started(1, 1));
  assert.ok(result.events.some(event => event.kind === 'Finalized'));
  assert.equal(result.retained, true);
});

test('events are available synchronously before the bus is dropped', async () => {
  const begin = started(1, 1);
  const next = worker('WorkerStarted');
  const results = await drive(1, emit(begin), receive(0, true), emit(next), receive(0, true), dropSender());
  assert.deepEqual(results[1], begin);
  assert.deepEqual(results[3], next);
});

test('a concurrent consumer observes the whole lifecycle while execution yields', async () => {
  const events = [started(3, 1), worker('WorkerStarted'), worker('WorkerFinished'), finished(3, 3, 0, 0, 50)];
  const actions = events.flatMap(event => [emit(event), { kind: 'yield' }]);
  const results = await drive(1, { kind: 'drain', subscriber: 0 }, ...actions, dropSender(),
    { kind: 'join', subscriber: 0 });
  assert.deepEqual(results.at(-1), events);
});
