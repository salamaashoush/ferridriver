import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, repo, run, workspace } from './support.mjs';

async function bdd(feature, flags = []) {
  const fixtures = join(repo, 'crates/ferridriver-cli/tests/fixtures/bdd');
  const cwd = await workspace({
    'features/probe.feature': await readFile(join(fixtures, `${feature}.feature`), 'utf8'),
    'steps/steps.js': await readFile(join(fixtures, 'steps.js'), 'utf8'),
  });
  return run(['bdd', '--no-inherit', '--headless', '--workers', '1', '--steps', 'steps/*.js', ...flags, 'features/'], { cwd });
}

test('undefined BDD steps fail and name the missing definition', async () => {
  const result = await bdd('pending_steps');
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /undefined step/);
});

test('no-strict reports undefined steps without failing the run', async () => {
  passed(await bdd('pending_steps', ['--no-strict']));
});

test('ambiguous BDD definitions fail even under no-strict', async () => {
  const result = await bdd('ambiguous_steps', ['--no-strict']);
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /ambiguous step/);
});

test('custom parameter regexes and transformers reach the step body', async () => {
  passed(await bdd('custom_param_types'));
});

test('a BDD scenario with only data fixtures needs no installed browser', async () => {
  const cwd = await workspace({
    'data.feature': `Feature: Totals
      Scenario Outline: Adding quantities
        Given the quantities <first> and <second>
        Then their total is <total>
        Examples:
          | first | second | total |
          | 2     | 3      | 5     |
          | 7     | 4      | 11    |
    `,
    'steps.js': `import { expect, test } from '@ferridriver/test';
      const fixtures = test.extend({
        order: async ({}, use) => { await use({ total: 0 }); },
      });
      const { Given, Then } = bindSteps(fixtures);
      Given('the quantities {int} and {int}', ({ order }, first, second) => { order.total = first + second; });
      Then('their total is {int}', ({ order }, total) => { expect(order.total).toBe(total); });
    `,
    'ferridriver.toml': `[test]
      workers = 2
      steps = ["steps.js"]
      [test.browser]
      executablePath = "/nonexistent/ferridriver-browser-probe"
      headless = true
    `,
  });
  passed(await run(['bdd', '--no-inherit', 'data.feature'], { cwd }));
});

test('BDD executes tables and outline rows, filters tags, and maps failing step sources', async () => {
  const fixtures = join(repo, 'crates/ferridriver-bdd/tests/fixtures');
  const cwd = await workspace({
    'cukes.feature': await readFile(join(fixtures, 'cukes.feature'), 'utf8'),
    'cukes.steps.js': await readFile(join(fixtures, 'cukes.steps.js'), 'utf8'),
    'ferridriver.toml': `[test]
      workers = 2
      steps = ["cukes.steps.js"]
      reporter = [{ name = "cucumber-json", outputFile = "report.json" }]
      [test.browser]
      headless = true
    `,
  });
  const result = await run(['bdd', '--no-inherit', '--tags', '@smoke and not @wip', 'cukes.feature'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  const report = JSON.parse(await readFile(join(cwd, 'report.json'), 'utf8'));
  const scenarios = report.flatMap(feature => feature.elements);
  assert.equal(scenarios.length, 5);
  assert.ok(scenarios.every(scenario => !scenario.name.includes('excluded')));
  const failures = scenarios.filter(scenario => scenario.steps.some(step => step.result.status === 'failed'));
  assert.equal(failures.length, 1);
  assert.equal(failures[0].name, 'deliberately failing');
  const failure = failures[0].steps.find(step => step.result.status === 'failed');
  assert.match(failure.result.error_message, /boom from js step/);
  assert.match(failure.result.error_message, /cukes.steps.js/);
  for (const name of ['eat some cukes', 'data table sum', 'Example #1', 'Example #2']) {
    const scenario = scenarios.find(scenario => scenario.name === name);
    assert.ok(scenario, `missing scenario ${name}`);
    assert.ok(scenario.steps.every(step => step.result.status === 'passed'), JSON.stringify(scenario));
  }
});

test('BDD step hooks run around failed steps and never around skipped steps', async () => {
  const fixtures = join(repo, 'crates/ferridriver-bdd/tests/fixtures');
  const cwd = await workspace({
    'hooks.feature': await readFile(join(fixtures, 'step_hooks.feature'), 'utf8'),
    'hooks.steps.js': await readFile(join(fixtures, 'step_hooks.steps.js'), 'utf8'),
    'ferridriver.toml': `[test]
      workers = 1
      steps = ["hooks.steps.js"]
      reporter = [{ name = "cucumber-json", outputFile = "report.json" }]
      [test.browser]
      headless = true
    `,
  });
  const result = await run(['bdd', '--no-inherit', 'hooks.feature'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  const report = JSON.parse(await readFile(join(cwd, 'report.json'), 'utf8'));
  const scenarios = report.flatMap(feature => feature.elements);
  assert.equal(scenarios.length, 2);
  const first = scenarios.find(scenario => scenario.name === 'hooks fire around every executed step');
  assert.ok(first.steps.some(step => step.result.status === 'failed'));
  assert.equal(first.steps.filter(step => !step.hidden && step.result.status === 'skipped').length, 1);
  const counters = scenarios.find(scenario => scenario.name === 'counters observed');
  assert.ok(counters.steps.every(step => step.result.status === 'passed'), JSON.stringify(counters));
});

for (const [directory, feature, count] of [['ts', 'calc.feature', 1], ['ts_expect', 'assert.feature', 3]]) {
  test(`TypeScript BDD ${directory} executes every scenario through native bindings`, async () => {
    const fixtures = join(repo, 'crates/ferridriver-bdd/tests/fixtures', directory);
    const files = {
      [feature]: await readFile(join(fixtures, feature), 'utf8'),
      'steps.ts': await readFile(join(fixtures, 'steps.ts'), 'utf8'),
      'ferridriver.toml': `[test]
        workers = 3
        steps = ["steps.ts"]
        reporter = [{ name = "cucumber-json", outputFile = "report.json" }]
        [test.browser]
        headless = true
      `,
    };
    if (directory === 'ts') files['math.ts'] = await readFile(join(fixtures, 'math.ts'), 'utf8');
    const cwd = await workspace(files);
    passed(await run(['bdd', '--no-inherit', feature], { cwd }));
    const scenarios = JSON.parse(await readFile(join(cwd, 'report.json'), 'utf8')).flatMap(feature => feature.elements);
    assert.equal(scenarios.length, count);
    for (const scenario of scenarios) assert.ok(scenario.steps.every(step => step.result.status === 'passed'), scenario.name);
  });
}
