import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

async function execute(source) {
  const { results } = await runtimeProbe([
    { op: 'configure-context', host: 'bdd' },
    { op: 'run-engine', source },
  ]);
  observation(results[0]);
  return observation(results[1]);
}

test('bound BDD steps expose the Cucumber registration surface', async () => {
  const result = await execute('return Object.keys(bindSteps(ferridriver.test)).sort()');
  assert.equal(result.status, 'ok', JSON.stringify(result));
  for (const name of ['After', 'AfterAll', 'AfterStep', 'And', 'Before', 'BeforeAll',
    'BeforeStep', 'But', 'Given', 'Step', 'Then', 'When', 'defineStep']) {
    assert.ok(result.value.includes(name), `missing ${name}: ${JSON.stringify(result.value)}`);
  }
});

for (const [title, source] of [
  ['registers a bound step', `const { Given } = bindSteps(ferridriver.test);
    Given('I have {int} cukes', function () {}); return 'ok';`],
  ['registers independent fixture chains and ambient steps', `
    const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
    const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
    bindSteps(a).Given('from a', function () {});
    bindSteps(b).Given('from b', function () {});
    Given('ambient', function () {}); return 'ok';`],
  ['registers steps and hooks from merged fixture chains', `
    const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
    const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
    const { Given, Before } = bindSteps(ferridriver.mergeTests(a, b));
    Given('needs both', async function ({ alpha, beta }) {});
    Before(async function ({ alpha }) {}); return 'ok';`],
]) {
  test(`BDD ${title}`, async () => {
    const result = await execute(source);
    assert.equal(result.status, 'ok', JSON.stringify(result));
    assert.equal(result.value, 'ok');
  });
}

for (const argument of ['{}', '5', 'undefined', '(() => {})']) {
  test(`BDD refuses to bind steps to ${argument}`, async () => {
    const result = await execute(`bindSteps(${argument}); return 'unreached';`);
    assert.equal(result.status, 'error', JSON.stringify(result));
    assert.ok(result.error.message.includes('accepts a "test" function'), JSON.stringify(result));
  });
}
