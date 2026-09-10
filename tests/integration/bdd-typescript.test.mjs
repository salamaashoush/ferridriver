import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, passed, repo, run, runtimeProbe, workspace } from './support.mjs';

test('TypeScript BDD steps retain used imports, discard unused code, and execute correctly', async () => {
  const files = {};
  for (const name of ['steps.ts', 'math.ts', 'calc.feature']) {
    files[name] = await readFile(join(repo, 'crates/ferridriver-bdd/tests/fixtures/ts', name), 'utf8');
  }
  const { results } = await runtimeProbe([{ op: 'bundle-source', entries: ['steps.ts'] }], files);
  const code = observation(results[0]).code;
  assert.ok(!code.includes('TREE_SHAKE_ME_AWAY_MARKER_9F3A'));
  assert.ok(code.includes('add'));
  assert.ok(!code.includes('interface Wallet'));
  const cwd = await workspace({ ...files, 'ferridriver.toml': `[test]
    steps = ["steps.ts"]
    reporter = [{ name = "cucumber-json", outputFile = "report.json" }]
    [test.browser]
    headless = true
  ` });
  passed(await run(['bdd', '--no-inherit', '--headless', 'calc.feature'], { cwd }));
  const report = JSON.parse(await readFile(join(cwd, 'report.json'), 'utf8'));
  const scenarios = report.flatMap(feature => feature.elements);
  assert.equal(scenarios.length, 1);
  assert.ok(scenarios[0].steps.every(step => step.result.status === 'passed'), JSON.stringify(scenarios));
});
