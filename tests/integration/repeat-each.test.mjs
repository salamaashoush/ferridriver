import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

async function repeat(body, options = {}) {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\nworkers = 1\nretries = 0\n',
    'repeat.test.mjs': `import { test, expect } from '@ferridriver/test';
      import { readFile, writeFile } from 'node:fs/promises';
      test('repeated case', async () => {
        let previous = [];
        try { previous = JSON.parse(await readFile('executions.json', 'utf8')); }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        const execution = previous.length;
        previous.push({ repeat: test.info().repeatEachIndex, retry: test.info().retry, output: test.info().outputDir });
        await writeFile('executions.json', JSON.stringify(previous));
        ${body}
      });`,
  });
  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', '--headless',
    '--repeat-each', '2', ...(options.retries ? ['--retries', String(options.retries)] : [])], { cwd });
  const executions = JSON.parse(await readFile(join(cwd, 'executions.json'), 'utf8'));
  return { ...result, executions };
}

test('an earlier repetition failure cannot be hidden by a later pass', async () => {
  const result = await repeat('expect(execution).toBeGreaterThan(0);');
  assert.equal(result.executions.length, 2, result.text);
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /1 failed/);
  assert.match(result.text, /1 passed/);
});

test('repetitions expose distinct indices and artifact directories', async () => {
  const result = await repeat('expect(true).toBe(true);');
  assert.equal(result.code, 0, result.text);
  assert.deepEqual(result.executions.map(item => item.repeat), [0, 1]);
  assert.equal(new Set(result.executions.map(item => item.output)).size, 2);
  assert.match(result.text, /2 passed/);
});

test('retries retain their repetition identity and independent outcomes', async () => {
  const result = await repeat('expect(test.info().retry).toBe(1);', { retries: 1 });
  assert.equal(result.code, 0, result.text);
  assert.equal(result.executions.length, 4);
  for (const index of [0, 1]) {
    const attempts = result.executions.filter(item => item.repeat === index);
    assert.deepEqual(attempts.map(item => item.retry), [0, 1]);
  }
  assert.match(result.text, /2 flaky/);
});

test('repetitions run concurrently and reporters receive distinct planned cases', async () => {
  const cwd = await workspace({
    'ferridriver.toml': `[test]
      testMatch = ["*.test.mjs"]
      workers = 2
      retries = 0
      reporter = [{ name = "./reporter.mjs" }, { name = "list" }]
    `,
    'reporter.mjs': `import { writeFile } from 'node:fs/promises';
      export default class Reporter {
        cases = [];
        results = [];
        onBegin(config, suite) {
          this.cases = suite.allTests().map(test => ({ id: test.id, repeat: test.repeatEachIndex, title: test.title }));
        }
        onTestEnd(test, result) {
          this.results.push({ id: test.id, repeat: test.repeatEachIndex, status: result.status });
        }
        async onEnd(result) {
          await writeFile('report.json', JSON.stringify({ cases: this.cases, results: this.results, status: result.status }));
        }
      }`,
    'parallel.test.mjs': `import { test, expect } from '@ferridriver/test';
      import { readFile, writeFile } from 'node:fs/promises';
      test.beforeAll(async () => {
        const index = test.info().repeatEachIndex;
        await writeFile('hook-' + index, String(index));
      });
      test('both repetitions are active', async () => {
        const index = test.info().repeatEachIndex;
        expect(await readFile('hook-' + index, 'utf8')).toBe(String(index));
        await writeFile('ready-' + index, String(test.info().workerIndex));
        await expect(async () => {
          const other = await readFile('ready-' + (1 - index), 'utf8');
          expect(other).not.toBe(String(test.info().workerIndex));
        }).toPass({ timeout: 5000, intervals: [10] });
      });`,
  });
  const result = await run(['test', '--no-inherit', '--headless', '--repeat-each', '2'], { cwd });
  assert.equal(result.code, 0, result.text);
  const report = JSON.parse(await readFile(join(cwd, 'report.json'), 'utf8'));
  assert.equal(report.status, 'passed');
  assert.deepEqual(report.cases.map(item => item.repeat).sort(), [0, 1]);
  assert.equal(new Set(report.cases.map(item => item.id)).size, 2);
  assert.ok(report.cases.every(item => item.title === 'both repetitions are active'));
  assert.equal(report.results.length, 2);
  for (const planned of report.cases) {
    const actual = report.results.find(item => item.id === planned.id);
    assert.deepEqual(actual, { id: planned.id, repeat: planned.repeat, status: 'passed' });
  }
});

test('each repetition starts with fresh JavaScript module state', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\nworkers = 1\nretries = 0\n',
    'module.test.mjs': `import { test, expect } from '@ferridriver/test';
      let executions = 0;
      test('module starts fresh', async () => {
        executions++;
        expect(executions).toBe(1);
      });`,
  });
  const result = await run(['test', '--no-inherit', '--headless', '--repeat-each', '2'], { cwd });
  assert.equal(result.code, 0, result.text);
  assert.match(result.text, /2 passed/);
});

test('repetitions release worker fixtures before initializing the next one', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\nworkers = 1\nretries = 0\n',
    'fixture.test.mjs': `import { test as base, expect } from '@ferridriver/test';
      import { readFile, writeFile } from 'node:fs/promises';
      async function record(event, index) {
        let events = [];
        try { events = JSON.parse(await readFile('lifecycle.json', 'utf8')); }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        events.push({ event, index });
        await writeFile('lifecycle.json', JSON.stringify(events));
      }
      const test = base.extend({
        state: [async ({}, use) => {
          const index = test.info().repeatEachIndex;
          await record('setup', index);
          await use({ used: false });
          await record('teardown', index);
        }, { scope: 'worker' }],
      });
      test('worker state starts fresh', async ({ state }) => {
        expect(state.used).toBe(false);
        state.used = true;
      });`,
  });
  const result = await run(['test', '--no-inherit', '--headless', '--repeat-each', '2'], { cwd });
  assert.equal(result.code, 0, result.text);
  assert.match(result.text, /2 passed/);
  const events = JSON.parse(await readFile(join(cwd, 'lifecycle.json'), 'utf8'));
  assert.deepEqual(events, [
    { event: 'setup', index: 0 }, { event: 'teardown', index: 0 },
    { event: 'setup', index: 1 }, { event: 'teardown', index: 1 },
  ]);
});

test('BDD repetitions finish hooks and worker fixtures before the next scenario', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\nworkers = 1\nrepeatEach = 2\nsteps = ["steps.mjs"]\n',
    'repeat.feature': 'Feature: Repetition isolation\n  Scenario: Fresh worker state\n    Given a fresh worker fixture\n',
    'steps.mjs': `import { readFile, writeFile } from 'node:fs/promises';
      async function record(event) {
        let events = [];
        try { events = JSON.parse(await readFile('events.json', 'utf8')); }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        events.push(event);
        await writeFile('events.json', JSON.stringify(events));
      }
      const test = ferridriver.test.extend({
        state: [async ({}, use) => {
          await record('setup');
          await use({ used: false });
          await record('teardown');
        }, { scope: 'worker' }],
      });
      bindSteps(test).Given('a fresh worker fixture', async function ({ state }) {
        if (state.used) throw new Error('worker fixture was reused');
        state.used = true;
        await record('step');
      });
      AfterAll(async () => { await record('afterAll'); });`,
  });
  const result = await run(['bdd', '--no-inherit', '--headless', 'repeat.feature'], { cwd });
  assert.equal(result.code, 0, result.text);
  assert.match(result.text, /2 passed/);
  const events = JSON.parse(await readFile(join(cwd, 'events.json'), 'utf8'));
  assert.deepEqual(events, ['setup', 'step', 'afterAll', 'teardown', 'setup', 'step', 'afterAll', 'teardown']);
});

test('worker fixture cleanup can use its browser before repetition shutdown', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\nworkers = 1\nretries = 0\n',
    'browser.test.mjs': `import { test as base, expect } from '@ferridriver/test';
      import { writeFile } from 'node:fs/promises';
      const test = base.extend({
        tab: [async ({ browser }, use) => {
          const index = test.info().repeatEachIndex;
          const worker = test.info().workerIndex;
          const context = await browser.newContext();
          const page = await context.newPage();
          await use(page);
          const title = await page.title();
          await context.close();
          await writeFile('closed-' + index + '.json', JSON.stringify({ title, worker }));
        }, { scope: 'worker' }],
      });
      test('leaves its page available for fixture cleanup', async ({ tab }) => {
        await tab.setContent('<title>Repetition ' + test.info().repeatEachIndex + '</title>');
        expect(await tab.title()).toBe('Repetition ' + test.info().repeatEachIndex);
      });`,
  });
  const result = await run(['test', '--no-inherit', '--headless', '--repeat-each', '2'], { cwd });
  assert.equal(result.code, 0, result.text);
  assert.match(result.text, /2 passed/);
  const observations = [];
  for (const index of [0, 1]) {
    const observed = JSON.parse(await readFile(join(cwd, 'closed-' + index + '.json'), 'utf8'));
    assert.equal(observed.title, 'Repetition ' + index);
    observations.push(observed);
  }
  assert.notEqual(observations[0].worker, observations[1].worker);
});
