import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

for (const [workers, cap, expectedPeak] of [[2, 0, 2], [1, 0, 1], [4, 1, 4]]) {
  test(`projects share ${workers} workers with project cap ${cap}`, async () => {
    const root = await workspace({
      'budget.test.ts': `import { test, expect } from '@ferridriver/test';
        import { writeFileSync } from 'node:fs';
        for (let i = 0; i < 6; i++) test('request ' + i, async ({}, info) => {
          const start = Date.now();
          await new Promise(resolve => setTimeout(resolve, 100));
          writeFileSync(info.project.name + '-' + i + '.interval', JSON.stringify([start, Date.now()]));
        });`,
      'ferridriver.toml': `[test]
        testMatch = ["*.test.ts"]
        workers = ${workers}
        maxParallelProjects = ${cap}
        reporter = [{ name = "null" }]
        [[test.projects]]
        name = "one"
        [[test.projects]]
        name = "two"
        [[test.projects]]
        name = "three"
      `,
    });
    passed(await run(['test', '--no-inherit'], { cwd: root }));
    const files = (await readdir(root)).filter(file => file.endsWith('.interval'));
    assert.equal(files.length, 18, 'every test in every project must run');
    const events = [];
    for (const file of files) {
      const [start, end] = JSON.parse(await readFile(join(root, file), 'utf8'));
      events.push([start, 1], [end, -1]);
    }
    events.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    let active = 0, peak = 0;
    for (const [, change] of events) { active += change; peak = Math.max(peak, active); }
    assert.equal(peak, expectedPeak, 'fill the worker budget without multiplying it per project');
  });
}

test('parallel projects keep their output files in distinct directories', async () => {
  const root = await workspace({
    'output.test.ts': `import { test } from '@ferridriver/test';
      import { writeFileSync } from 'node:fs';
      test('same test', async ({}, info) => {
        const index = info.project.name === 'one/two' ? 0 : 1;
        const output = info.outputPath('result.txt');
        writeFileSync(output, info.project.name);
        writeFileSync(index + '.json', JSON.stringify({ output, project: info.project.name }));
      });`,
    'ferridriver.toml': `[test]
      testMatch = ["*.test.ts"]
      workers = 2
      maxParallelProjects = 0
      [[test.projects]]
      name = "one/two"
      [[test.projects]]
      name = "one-two"
    `,
  });
  passed(await run(['test', '--no-inherit'], { cwd: root }));
  const outputs = await Promise.all([0, 1].map(async index => JSON.parse(await readFile(join(root, `${index}.json`), 'utf8'))));
  assert.equal(new Set(outputs.map(result => result.output)).size, 2, 'project names must not collide after sanitizing');
  for (const result of outputs) assert.equal(await readFile(result.output, 'utf8'), result.project);
});

async function scheduledProjects(names, durations, cap = 0, dependencies = {}) {
  const root = await workspace({
    'schedule.test.ts': `import { test } from '@ferridriver/test';
      import { writeFileSync } from 'node:fs';
      for (const [name, duration] of Object.entries(${JSON.stringify(durations)})) {
        test(name, async () => {
          const start = Date.now();
          await new Promise(resolve => setTimeout(resolve, duration));
          writeFileSync(name + '.interval', JSON.stringify([start, Date.now()]));
        });
      }`,
    'ferridriver.toml': `[test]
      testMatch = ["*.test.ts"]
      workers = 8
      maxParallelProjects = ${cap}
      reporter = [{ name = "null" }]
      ${names.map(name => `[[test.projects]]
        name = "${name}"
        grep = "> ${name}$"
        dependencies = ${JSON.stringify(dependencies[name] ?? [])}`).join('\n')}
    `,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd: root });
  passed(result);
  const files = (await readdir(root)).filter(file => file.endsWith('.interval'));
  assert.equal(files.length, names.length);
  const intervals = await Promise.all(names.map(async name => JSON.parse(await readFile(join(root, `${name}.interval`), 'utf8'))));
  return { elapsed: result.elapsed, intervals };
}

test('independent projects execute concurrently within the slowest-project budget', async () => {
  const { elapsed, intervals } = await scheduledProjects(['a', 'b', 'c', 'd'], { a: 400, b: 400, c: 400, d: 400 });
  assert.ok(elapsed < 800, `four 400 ms projects should finish concurrently, took ${elapsed} ms`);
  assert.ok(Math.max(...intervals.map(([start]) => start)) < Math.min(...intervals.map(([, end]) => end)), 'all independent projects must overlap');
});

test('a project parallelism cap of one serializes independent projects', async () => {
  const { elapsed, intervals } = await scheduledProjects(['a', 'b', 'c'], { a: 250, b: 250, c: 250 }, 1);
  assert.ok(elapsed >= 500, `serial projects cannot complete in one interval, took ${elapsed} ms`);
  intervals.sort(([a], [b]) => a - b);
  for (let index = 1; index < intervals.length; index++) {
    assert.ok(intervals[index][0] >= intervals[index - 1][1], 'a capped project must finish before the next starts');
  }
});

test('dependent projects start after their dependency completes', async () => {
  const { elapsed, intervals: [setup, app] } = await scheduledProjects(['setup', 'app'], { setup: 500, app: 200 }, 0, { app: ['setup'] });
  assert.ok(elapsed >= 600, `the dependent project cannot overlap setup, took ${elapsed} ms`);
  assert.ok(app[0] >= setup[1], 'app must start after setup finishes');
});
