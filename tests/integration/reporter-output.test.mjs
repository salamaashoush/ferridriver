import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const id = (name, file = 'tests/login.spec.ts', suite = 'tests/login.spec.ts::auth', line = 12, column = 5) =>
  ({ file, suite, name, line, column });
const outcome = (name, status = 'passed', attempt = 1) => ({
  test_id: id(name), status, duration_ms: 250, attempt, max_attempts: 2, project_name: 'chromium',
  worker_index: 2, parallel_index: 2, start_time_ms: 1700000000000, timeout_ms: 30000,
  annotations: [{ tag: '@smoke' }, { info: { type_name: 'issue', description: 'JIRA-1' } }],
});
function failed(name, attempt = 1) {
  const error = {
    message: 'Error: expect(locator).toHaveText(expected) failed',
    stack: '    at check (tests/login.spec.ts:19:7)', diff: 'Expected: "hi"\nReceived: "bye"', screenshot: 'image',
  };
  return { ...outcome(name, 'failed', attempt), error, errors: [error], stdout: 'trying to sign in\n',
    steps: [{ step_id: 's1', title: 'sign in', category: 'test.step', duration_ms: 80, status: 'failed',
      error: 'no such button', location: { file: 'tests/login.spec.ts', line: 19, column: 0 } }],
    attachments: [{ name: 'screenshot', content_type: 'image/png', resource: 'image' }],
  };
}
const start = (total_tests = 2, num_workers = 2) =>
  ({ kind: 'run-started', total_tests, num_workers, metadata: { ci: 'local' }, start_time_ms: 1700000000000 });
const finish = (total = 2, passed = 1, failed = 1, skipped = 0, duration_ms = 900) =>
  ({ kind: 'run-finished', total, passed, failed, skipped, flaky: 0, duration_ms, status: 'failed' });
const testFinished = outcome => ({ kind: 'test-finished', outcome });
const runEvents = outcomes => [start(), ...outcomes.map(testFinished), finish()];
const examples = () => [outcome('logs in'), failed('rejects a bad password')];

async function report(names, events, { mode = 'factory', files = {}, shard, options = {} } = {}) {
  const { results, cwd } = await runtimeProbe([{
    op: 'reporter-output', mode, shard, events, resources: { image: [137, 80, 78, 71] },
    config: { outputDir: 'reports', testDir: 'tests', quiet: true,
      reporter: names.map(name => ({ name, ...options })) },
  }], files);
  return { ...observation(results[0]), cwd,
    read: name => readFile(join(cwd, 'reports', name), 'utf8'),
    exists: async name => {
      try { await stat(join(cwd, 'reports', name)); return true; }
      catch (error) { if (error.code === 'ENOENT') return false; throw error; }
    },
  };
}

function simple(name, status, message) {
  const error = message ? { message } : null;
  return { test_id: id(name, 'tests/reporters.rs', null, 42, null), status,
    duration_ms: 10, attempt: 1, max_attempts: 1, error, errors: error ? [error] : [] };
}

test('the dot reporter renders one glyph for each completed test and its failure message', async () => {
  const result = await report([], [start(3, 1), ...[
    simple('t1', 'passed'), simple('t2', 'failed', 'boom'), simple('t3', 'skipped'),
  ].map(testFinished), finish(3, 1, 1, 1, 30)], { mode: 'dot' });
  assert.ok(result.text.includes('\u00b7F\u00b0'), result.text);
  assert.ok(result.text.includes('boom'), result.text);
});

test('the empty reporter emits no output and finalizes normally', async () => {
  const result = await report([], [start(0, 0)], { mode: 'empty' });
  assert.equal(result.text, '');
});

test('the enabled GitHub reporter escapes annotation newlines and forwards its event', async () => {
  const result = await report([], [testFinished(simple('crash', 'failed', 'boom\nwith\nnewlines'))], { mode: 'github' });
  assert.ok(result.text.includes('::error '), result.text);
  assert.ok(result.text.includes('boom%0Awith%0Anewlines'), result.text);
  assert.deepEqual(result.delegated.map(event => event.kind), ['TestFinished', 'Finalized']);
});

test('the blob reporter writes a shard archive that the production merge reader replays in order', async () => {
  const testId = id('blob-roundtrip', 'tests/reporters.rs', null, 42, null);
  const result = await report([], [
    { ...start(1, 1), metadata: { key: 'value' } },
    { kind: 'test-started', test_id: testId, project: '', attempt: 1, worker_id: 0 },
    testFinished(simple('blob-roundtrip', 'passed')), finish(1, 1, 0, 0, 7),
  ], { mode: 'blob', shard: [1, 2] });
  assert.equal(await result.exists('report-1.zip'), true);
  assert.deepEqual(result.events.map(event => event.kind), ['RunStarted', 'TestStarted', 'TestFinished', 'RunFinished']);
});

test('JSON reports preserve the Playwright shape and complete failure diagnostics', async () => {
  const output = await report(['json'], runEvents(examples()));
  const json = JSON.parse(await output.read('results.json'));
  for (const key of ['config', 'suites', 'errors', 'stats']) assert.ok(Object.hasOwn(json, key), key);
  assert.equal(json.stats.expected, 1);
  assert.equal(json.stats.unexpected, 1);
  assert.equal(json.stats.startTime, '2023-11-14T22:13:20.000Z');
  assert.equal(json.config.workers, output.defaultWorkers);
  assert.equal(json.config.projects[0].name, '');
  const suite = json.suites[0];
  assert.equal(suite.file, 'tests/login.spec.ts');
  const spec = suite.specs[0];
  assert.equal(spec.title, 'logs in');
  assert.equal(spec.ok, true);
  assert.equal(spec.line, 12);
  assert.equal(spec.column, 5);
  assert.equal(spec.tags[0], 'smoke');
  const first = spec.tests[0];
  assert.equal(first.projectName, 'chromium');
  assert.equal(first.expectedStatus, 'passed');
  assert.equal(first.status, 'expected');
  assert.equal(first.timeout, 30000);
  const result = first.results[0];
  assert.equal(result.workerIndex, 2);
  assert.equal(result.parallelIndex, 2);
  assert.equal(result.retry, 0);
  assert.equal(result.startTime, '2023-11-14T22:13:20.000Z');
  assert.equal(result.status, 'passed');
  const failing = suite.specs[1].tests[0].results[0];
  assert.equal(failing.status, 'failed');
  assert.equal(failing.errorLocation.file, 'tests/login.spec.ts');
  assert.equal(failing.errorLocation.line, 19);
  assert.equal(failing.errorLocation.column, 7);
  assert.equal(failing.errors.length, 1);
  assert.equal(failing.stdout[0].text, 'trying to sign in\n');
  assert.equal(failing.steps[0].title, 'sign in');
  assert.equal(failing.attachments[0].body, 'iVBORw==');
});

test('JUnit reports carry classifications annotations timestamps and existing attachment paths', async () => {
  const rejected = failed('rejects a bad password');
  rejected.attachments.push({ name: 'trace', content_type: 'application/zip', path: 'trace.zip' });
  const output = await report(['junit'], runEvents([outcome('logs in'), rejected]), {
    files: { 'trace.zip': 'not really a trace' },
  });
  const xml = await output.read('junit.xml');
  for (const fragment of [
    '<testsuites id="" name="" tests="2" failures="1" skipped="0" errors="0"',
    'hostname="chromium"', 'timestamp="', 'classname="tests/login.spec.ts"',
    '<property name="tag" value="@smoke"/>', '<property name="issue" value="JIRA-1"/>',
    'type="expect.toHaveText"', '<![CDATA[', '[[ATTACHMENT|',
  ]) assert.ok(xml.includes(fragment), `${fragment}\n${xml}`);
});

test('CTRF and Markdown reports retain run counts and failure details', async () => {
  const output = await report(['ctrf', 'markdown'], runEvents(examples()));
  const json = JSON.parse(await output.read('ctrf-report.json'));
  assert.equal(json.results.tool.name, 'ferridriver');
  assert.equal(json.results.summary.tests, 2);
  assert.equal(json.results.summary.passed, 1);
  assert.equal(json.results.summary.failed, 1);
  const tests = json.results.tests;
  assert.equal(tests[0].name, 'logs in');
  assert.equal(tests[0].status, 'passed');
  assert.equal(tests[0].browser, 'chromium');
  assert.equal(tests[1].status, 'failed');
  assert.equal(tests[1].line, 12);
  assert.ok(tests[1].message.includes('toHaveText'));
  const markdown = await output.read('report.md');
  for (const text of ['| Passed | Failed | Flaky | Skipped | Duration |', '| 1 | 1 | 0 | 0 |',
    '### Failures', 'rejects a bad password']) assert.ok(markdown.includes(text), markdown);
});

test('a retried test is flaky in JSON and CTRF while both attempts remain available', async () => {
  const output = await report(['json', 'ctrf'], runEvents([failed('logs in'), outcome('logs in', 'passed', 2)]));
  const json = JSON.parse(await output.read('results.json'));
  assert.equal(json.stats.flaky, 1);
  assert.equal(json.stats.unexpected, 0);
  const spec = json.suites[0].specs[0];
  assert.equal(spec.tests[0].status, 'flaky');
  assert.equal(spec.tests[0].results.length, 2);
  assert.equal(spec.ok, true);
  const ctrf = JSON.parse(await output.read('ctrf-report.json'));
  assert.equal(ctrf.results.tests[0].flaky, true);
  assert.equal(ctrf.results.tests[0].status, 'passed');
  assert.equal(ctrf.results.tests[0].retries, 1);
});

test('a declared expected failure is classified as expected in JSON', async () => {
  const known = { ...failed('known bug'), expected_failure: true };
  const output = await report(['json'], runEvents([known]));
  const json = JSON.parse(await output.read('results.json'));
  assert.equal(json.stats.expected, 1);
  assert.equal(json.stats.unexpected, 0);
  assert.equal(json.suites[0].specs[0].tests[0].expectedStatus, 'failed');
  assert.equal(json.suites[0].specs[0].ok, true);
});

test('run errors retain their message and stack location in JSON reports', async () => {
  const output = await report(['json'], [start(), { kind: 'run-error', error: {
    message: 'global setup failed: boom', stack: '    at setup (global.ts:4:2)',
  } }, finish()]);
  const json = JSON.parse(await output.read('results.json'));
  assert.equal(json.errors.length, 1);
  assert.ok(json.errors[0].message.includes('boom'));
  assert.equal(json.errors[0].location.file, 'global.ts');
  assert.equal(json.errors[0].location.line, 4);
});

test('the outputFile option selects the report path and leaves the default path absent', async () => {
  const output = await report(['junit'], runEvents([outcome('logs in')]), {
    options: { outputFile: 'reports/custom/where.xml' },
  });
  assert.equal(await output.exists('custom/where.xml'), true);
  assert.equal(await output.exists('junit.xml'), false);
});
