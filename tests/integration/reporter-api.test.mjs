import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const example = (name, describe = null) =>
  ({ file: 'tests/pay.spec.ts', describe, name, line: 7, column: 1, tags: ['smoke'] });
const cases = [example('adds a row', 'Checkout'), example('loads')];

async function preamble(groups = [cases], project = null) {
  const { results } = await runtimeProbe([{
    op: 'reporter-api', action: 'preamble', config: { testDir: 'tests', timeout: 30000 },
    projectName: 'chromium', project, groups,
  }]);
  return observation(results[0]);
}

test('reporter suites retain root project file and describe nesting with stable case IDs', async () => {
  const result = await preamble([cases], { name: 'chromium' });
  const root = result.preamble.suite;
  assert.equal(root.type, 'root');
  assert.equal(root.title, '');
  assert.equal(root.suites.length, 1);
  const project = root.suites[0];
  assert.equal(project.type, 'project');
  assert.equal(project.title, 'chromium');
  assert.deepEqual(project.titlePath, ['', 'chromium']);
  assert.equal(project.project.name, 'chromium');
  const file = project.suites[0];
  assert.equal(file.type, 'file');
  assert.equal(file.title, 'tests/pay.spec.ts');
  assert.equal(file.location.file, 'pay.spec.ts');
  const describe = file.suites[0];
  assert.equal(describe.type, 'describe');
  assert.equal(describe.title, 'Checkout');
  assert.equal(describe.tests.length, 1);
  assert.equal(file.tests.length, 1);
  const nested = describe.tests[0];
  assert.equal(nested.title, 'adds a row');
  assert.deepEqual(nested.titlePath, ['', 'chromium', 'tests/pay.spec.ts', 'Checkout', 'adds a row']);
  assert.deepEqual(nested.tags, ['@smoke']);
  assert.equal(nested.id, result.caseIds[0]);
  assert.equal(result.totalCases, 2);
});

test('merging shard preambles unions cases without adding duplicates', async () => {
  const result = await preamble([[example('loads')], [example('saves')], [example('loads')]]);
  const file = result.preamble.suite.suites[0].suites[0];
  assert.deepEqual(file.tests.map(item => item.title), ['loads', 'saves']);
});

test('file-only reporting gains terminal output for runs but not shard merges', async () => {
  const configurations = [['json', false], ['json', true], ['list', false]];
  const { results } = await runtimeProbe(configurations.map(([name, merge]) => ({
    op: 'reporter-api', action: 'factory', reporters: [{ name }], merge,
  })));
  assert.deepEqual(results.map(result => observation(result).printsToStdio), [true, false, true]);
});

test('the complete reporter preamble round trips through the production JSON reader', async () => {
  const result = await preamble();
  assert.equal(result.roundTripCases, 2);
});

test('the preamble JSON contains no floating point number tokens', async () => {
  const { text } = await preamble();
  const tokens = text.match(/"(?:\\.|[^"\\])*"|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g) ?? [];
  const floats = tokens.filter(token => !token.startsWith('"') && /[.eE]/.test(token));
  assert.deepEqual(floats, [], `the blob wire cannot read these numbers: ${JSON.stringify(floats)}`);
});
