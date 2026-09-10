import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, repo, runtimeProbe } from './support.mjs';

const file = 'tests/pay.spec.ts';
const project = 'chromium';
const id = name => ({ file, suite: `${file}::Checkout`, name, line: 12, column: 3 });

async function preamble(names) {
  const { results } = await runtimeProbe([{
    op: 'reporter-api', action: 'preamble', config: {}, projectName: project,
    groups: [names.map(name => ({ file, describe: 'Checkout', name, line: 12, column: 3, tags: ['@smoke'] }))],
  }]);
  const ids = observation(results[0]).caseIds;
  const cases = names.map((title, i) => ({ id: ids[i], title,
    titlePath: ['', project, file, 'Checkout', title], location: { file, line: 12, column: 3 },
    expectedStatus: 'passed', timeout: 30000, retries: 0, repeatEachIndex: 0,
    tags: ['@smoke'], annotations: [], projectName: project,
  }));
  const describe = { title: 'Checkout', type: 'describe', titlePath: ['', project, file, 'Checkout'], suites: [], tests: cases };
  const source = { title: file, type: 'file', titlePath: ['', project, file], location: { file, line: 0, column: 0 }, suites: [describe], tests: [] };
  const engine = { title: project, type: 'project', titlePath: ['', project], project: { name: project, id: project }, suites: [source], tests: [] };
  return { config: { rootDir: '/repo', projects: [{ name: project }] },
    suite: { title: '', type: 'root', titlePath: [''], suites: [engine], tests: [] } };
}

function events(tree, names, status = 'passed') {
  return [
    { kind: 'run-started', total_tests: names.length, num_workers: 1, metadata: null, start_time_ms: 1700000000000, preamble: tree },
    ...names.flatMap(name => {
      const shared = { test_id: id(name), project };
      const step = { ...shared, step_id: 'step-1', title: 'open the cart', category: 'test.step' };
      return [
        { kind: 'test-started', ...shared, attempt: 1, worker_id: 0 },
        { kind: 'step-started', ...step },
        { kind: 'test-output', ...shared, stderr: false, text: 'a line the test printed' },
        { kind: 'step-finished', ...step, duration_ms: 40 },
        { kind: 'test-finished', outcome: {
          test_id: id(name), status, duration_ms: 120, attempt: 1, max_attempts: 1,
          error: status === 'passed' ? null : { message: 'it did not add up' },
          attachments: [{ name: 'screenshot', content_type: 'image/png', resource: 'image' }],
          project_name: project, worker_index: 0, parallel_index: 0,
          start_time_ms: 1700000000000, timeout_ms: 30000,
        } },
      ];
    }),
    { kind: 'run-finished', total: names.length, passed: names.length, failed: 0, skipped: 0, flaky: 0, duration_ms: 900, status: 'passed' },
  ];
}

async function report(fixture, { status = 'passed', options = {}, names = ['adds a row'], preprocess = false, drive = true, before = [] } = {}) {
  const tree = await preamble(names);
  const files = {};
  for (const name of new Set([...before, fixture])) {
    if (name !== 'no-such-reporter') files[`${name}.ts`] = await readFile(join(repo, 'tests/fixtures', `${name}.ts`), 'utf8');
  }
  const operations = [...before, fixture].map(name => ({
    op: 'reporter-output', mode: 'js', config: { reporter: [{ name: `./${name}.ts`, outputFile: 'summary.json', ...options }] },
    resources: { image: [1, 2, 3, 4] }, events: drive ? events(tree, names, status) : [],
    preprocess: preprocess ? tree : undefined,
  }));
  const { results, cwd } = await runtimeProbe(operations, files);
  return { results, tree, read: async () => JSON.parse(await readFile(join(cwd, 'summary.json'), 'utf8')) };
}

test('JS reporter V1 receives complete Playwright event shapes', async () => {
  const result = await report('counting-reporter');
  observation(result.results[0]);
  const summary = await result.read();
  for (const hook of ['onBegin', 'onTestBegin', 'onStepBegin', 'onStepEnd', 'onTestEnd', 'onStdOut', 'onEnd']) assert.equal(summary.calls[hook], 1, hook);
  assert.equal(summary.configuredCalled, false);
  assert.equal(summary.configRootDir, '/repo');
  assert.equal(summary.configProjects[0], project);
  assert.equal(summary.suiteType, 'root');
  assert.deepEqual(summary.suiteTitlePath, ['']);
  assert.deepEqual(summary.allTests, [' > chromium > tests/pay.spec.ts > Checkout > adds a row']);
  assert.deepEqual(summary.entryTypes, ['project']);
  assert.equal(summary.firstProject, project);
  assert.equal(summary.testProject, project);
  assert.deepEqual(summary.statuses, ['passed']);
  assert.deepEqual(summary.outcomes, ['expected']);
  assert.deepEqual(summary.ok, [true]);
  assert.deepEqual(summary.stepTitles, ['open the cart']);
  assert.deepEqual(summary.stepPaths, [['open the cart']]);
  assert.deepEqual(summary.stdout, ['a line the test printed']);
  assert.equal(summary.attachments[0].name, 'screenshot');
  assert.equal(summary.attachments[0].contentType, 'image/png');
  assert.equal(summary.attachments[0].body, 'AQIDBA==');
  assert.equal(summary.status, 'passed');
  assert.equal(summary.durationIsNumber, true);
  assert.equal(summary.startTimeIsDate, true);
  assert.deepEqual(summary.errors, []);
});

test('JS reporter marks unexpected failures as unsuccessful', async () => {
  const result = await report('counting-reporter', { status: 'failed' });
  observation(result.results[0]);
  const summary = await result.read();
  assert.deepEqual(summary.statuses, ['failed']);
  assert.deepEqual(summary.outcomes, ['unexpected']);
  assert.deepEqual(summary.ok, [false]);
});

test('JS reporter V2 receives configuration separately from its suite', async () => {
  const result = await report('v2-reporter');
  observation(result.results[0]);
  assert.deepEqual(await result.read(), { rootDir: '/repo', beganType: 'root', beganWith: '1' });
});

test('JS reporter exceptions leave subsequent reporters operational', async () => {
  const result = await report('counting-reporter', { before: ['throwing-reporter'] });
  result.results.forEach(observation);
  assert.equal((await result.read()).calls.onEnd, 1);
});

test('JS reporter stdio defaults and explicit answers reach the host', async () => {
  for (const [fixture, options, expected] of [
    ['counting-reporter', {}, false], ['counting-reporter', { printsToStdio: true }, true], ['throwing-reporter', {}, true],
  ]) {
    const result = await report(fixture, { options, drive: false });
    assert.equal(observation(result.results[0]).printsToStdio, expected);
  }
});

test('JS reporter preprocessing returns stable-ID exclusions, annotations and sharding edits', async () => {
  const names = ['an excluded case', 'a skipped case', 'a plain case'];
  const result = await report('preprocess-reporter', { names, preprocess: true, drive: false });
  const { edits } = observation(result.results[0]);
  assert.deepEqual((await result.read()).seen, names);
  const cases = result.tree.suite.suites[0].suites[0].suites[0].tests;
  assert.deepEqual(edits.excluded, [cases[0].id]);
  assert.equal(edits.annotations.length, 1);
  assert.equal(edits.annotations[0][0], cases[1].id);
  assert.equal(edits.annotations[0][1].skip.reason, 'a reporter said so');
  assert.equal(edits.skipSharding, true);
});

test('JS reporter onEnd can override a passing run status', async () => {
  const result = await report('status-reporter', { options: { status: 'failed' } });
  assert.equal(observation(result.results[0]).status, 'failed');
});

test('JS reporter loading fails for an unresolved module', async () => {
  const result = await report('no-such-reporter', { drive: false });
  assert.ok(result.results[0].error.includes('no-such-reporter.ts'));
});
