import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

async function suite(source, actions) {
  const { results } = await runtimeProbe([{ op: 'bdd-session', entries: ['steps.ts'], actions }], { 'steps.ts': source });
  return observation(results[0]);
}

function scenario(steps, hooks = [], parameters = null) {
  return [{ action: 'begin', steps, hooks, parameters },
    ...hooks.map(index => ({ action: 'hook', index })),
    ...steps.map(index => ({ action: 'step', index })),
    { action: 'drain' }, { action: 'end' }];
}

function values(result) {
  return result.results.map(observation).map(value => {
    if (value?.outcome) assert.equal(value.outcome, 'Passed');
    assert.ok(!value?.error, JSON.stringify(value));
    return value;
  });
}

function logs(result) {
  return values(result).filter(Array.isArray).map(attachments => attachments.map(a => Buffer.from(a.bytes).toString()));
}

test('BDD steps resolve named fixtures and omit unrequested fixtures', async () => {
  const result = await suite(`const test = ferridriver.test.extend({
    token: async ({}, use) => { await use('t-42'); },
  });
  const { Given } = bindSteps(test);
  Given('reads token', async function ({ token }) { this.log(token); });
  Given('reads nothing', async function () { this.log('none'); });`,
  [...scenario([0]), ...scenario([1])]);
  assert.deepEqual(logs(result), [['t-42'], ['none']]);
});

test('BDD auto fixtures run without being requested by a step', async () => {
  const result = await suite(`const marks = [];
    const test = ferridriver.test.extend({ seeded: [async ({}, use) => {
      marks.push('setup'); await use(true);
    }, { auto: true }] });
    bindSteps(test).Given('runs', async function () { this.log(marks.join(',')); });`, scenario([0]));
  assert.deepEqual(logs(result), [['setup']]);
});

test('BDD tears down fixture dependencies in reverse order after the step', async () => {
  const result = await suite(`const order = [];
    const test = ferridriver.test.extend({
      outer: async ({}, use) => { order.push('outer up'); await use('o'); order.push('outer down'); },
      inner: async ({ outer }, use) => { order.push('inner up'); await use(outer + 'i'); order.push('inner down'); },
    });
    const { Given } = bindSteps(test);
    Given('uses both', async function ({ inner }) { this.log('step ' + inner); });
    Given('reports', async function () { this.log(order.join('|')); });`, [...scenario([0]), ...scenario([1])]);
  assert.deepEqual(logs(result), [['step oi'], ['outer up|inner up|inner down|outer down']]);
});

test('BDD caches worker fixtures across scenarios until worker teardown', async () => {
  const result = await suite(`const marks = [];
    const test = ferridriver.test.extend({ pool: [async ({}, use) => {
      marks.push('open'); await use(marks.length); marks.push('close');
    }, { scope: 'worker' }] });
    bindSteps(test).Given('uses pool', async function ({ pool }) {
      this.log('pool=' + pool + ' marks=' + marks.join(','));
    });`, [...scenario([0]), ...scenario([0]), { action: 'teardown-worker' }, ...scenario([0])]);
  assert.deepEqual(logs(result), [['pool=1 marks=open'], ['pool=1 marks=open'], ['pool=3 marks=open,close,open']]);
});

test('BDD shares the fixture bag as this and argument zero with the world prototype', async () => {
  const result = await suite(`setWorldConstructor(class World {
    constructor({ parameters }) { this.env = parameters.env; }
    greet() { return 'hi ' + this.env; }
  });
  const test = ferridriver.test.extend({ token: async ({}, use) => { await use('t'); } });
  const { Given } = bindSteps(test);
  Given('writes', async function (world) { world.seen = this.greet(); });
  Given('reads', async function ({ token }) {
    this.log(this.seen + ' ' + token + ' ' + (this === arguments[0]));
  });`, scenario([0, 1], [], { env: 'staging' }));
  assert.deepEqual(logs(result), [['hi staging t true']]);
});

test('BDD hooks resolve fixtures and share their writes with subsequent steps', async () => {
  const result = await suite(`const test = ferridriver.test.extend({ token: async ({}, use) => { await use('t-9'); } });
    const { Given, Before } = bindSteps(test);
    Before(async function ({ token }) { this.fromHook = token; });
    Given('reads hook', async function () { this.log(this.fromHook); });`, scenario([0], [0]));
  assert.deepEqual(logs(result), [['t-9']]);
});

test('BDD rejects unrelated fixture chains and accepts a scenario using one chain', async () => {
  const result = await suite(`const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
    const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
    bindSteps(a).Given('from a', async function ({ alpha }) {});
    bindSteps(b).Given('from b', async function ({ beta }) {});`,
  [{ action: 'plan', steps: [0, 1] }, { action: 'plan', steps: [0] }, ...scenario([0])]);
  assert.ok(result.results[0].error.includes('unrelated `test` objects'));
  assert.ok(result.results[0].error.includes('mergeTests'));
  const plan = observation(result.results[1]);
  assert.equal(plan.fixtureSet, result.stepFixtureSets[0]);
  assert.deepEqual(plan.requested, ['alpha']);
  values({ results: result.results.slice(2) });
});

test('BDD merged fixture chains serve steps bound to either half', async () => {
  const result = await suite(`const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
    const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
    bindSteps(ferridriver.mergeTests(a, b)).Given('both', async function ({ alpha, beta }) { this.log(alpha + beta); });
    bindSteps(a).Given('alpha', async function ({ alpha }) { this.log(alpha); });`, scenario([0, 1]));
  assert.deepEqual(logs(result), [['ab', 'a']]);
});

test('BDD drains typed attachments exactly once', async () => {
  const result = await suite(`Given('step', async function () {
    this.attach('hello', 'text/plain'); this.log('a note'); this.attach({ k: 1 });
  });`, [...scenario([0]), { action: 'drain' }]);
  assert.equal(result.steps, 1);
  const attachments = values(result).filter(Array.isArray);
  assert.deepEqual(attachments, [[
    { bytes: [...Buffer.from('hello')], mediaType: 'text/plain' },
    { bytes: [...Buffer.from('a note')], mediaType: 'text/x.cucumber.log+plain' },
    { bytes: [...Buffer.from('{"k":1}')], mediaType: 'application/json' },
  ], []]);
});

test('BDD After hooks receive the failing scenario result', async () => {
  const result = await suite(`After(function (world, s) {
    if (s.result.status === 'FAILED') this.attach('failed:' + s.pickle.name + ':' + s.result.message, 'text/plain');
  });`, [{ action: 'begin', steps: [] }, { action: 'hook', index: 0,
    argument: { name: 'My scenario', tags: ['@x'], status: 'FAILED', message: 'boom' } },
  { action: 'drain' }, { action: 'end' }]);
  assert.equal(result.hooks, 1);
  assert.deepEqual(values(result).filter(Array.isArray), [[
    { bytes: [...Buffer.from('failed:My scenario:boom')], mediaType: 'text/plain' },
  ]]);
});

test('BDD custom parameter transformers deliver typed objects to steps', async () => {
  const result = await suite(`defineParameterType({ name: 'amount', regexp: /\\d+/,
    transformer: s => ({ n: Number(s) * 2 }) });
    Given('I have {amount}', async function (world, a) { this.attach(JSON.stringify(a), 'application/json'); });`,
  [{ action: 'begin', steps: [0] }, { action: 'step', index: 0, args: [{ kind: 'custom', name: 'amount', raw: '21' }] },
    { action: 'drain' }, { action: 'end' }]);
  assert.equal(result.steps, 1);
  assert.equal(result.parameterTypes, 1);
  assert.deepEqual(logs(result), [['{"n":42}']]);
});

test('BDD definition wrappers run before and after the step body', async () => {
  const result = await suite(`setDefinitionFunctionWrapper(function (fn) {
    return async function (...a) { this.attach('before', 'text/plain');
      const r = await fn.apply(this, a); this.attach('after', 'text/plain'); return r; };
  });
  Given('s', async function () { this.attach('inner', 'text/plain'); });`, scenario([0]));
  assert.deepEqual(logs(result), [['before', 'inner', 'after']]);
});

test('BDD enforces the per-step timeout for an unresolved promise', async () => {
  const result = await suite(`Given('slow', { timeout: 30 }, async function () { await new Promise(() => {}); });`,
    [{ action: 'begin', steps: [0] }, { action: 'step', index: 0 }]);
  observation(result.results[0]);
  const step = observation(result.results[1]);
  assert.equal(step.error.kind, 'timeout');
});

test('BDD exposes world parameters without changing their types', async () => {
  const result = await suite(`Given('s', async function () {
    this.attach(JSON.stringify(this.parameters), 'application/json');
  });`, scenario([0], [], { env: 'staging', n: 3 }));
  const attachments = values(result).filter(Array.isArray);
  assert.equal(attachments.length, 1);
  assert.equal(attachments[0].length, 1);
  assert.deepEqual(JSON.parse(Buffer.from(attachments[0][0].bytes).toString()), { env: 'staging', n: 3 });
});

test('BDD skip returns a skipped outcome and stops executing the step body', async () => {
  const result = await suite(`Given('s', async function () {
    this.attach('pre', 'text/plain'); this.skip(); this.attach('post', 'text/plain');
  });`, scenario([0]));
  const results = result.results.map(observation);
  assert.equal(results[1].outcome, 'Skipped');
  assert.deepEqual(results[2], [{ bytes: [...Buffer.from('pre')], mediaType: 'text/plain' }]);
});
