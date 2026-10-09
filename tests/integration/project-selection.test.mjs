import assert from 'node:assert/strict';
import { readdir } from 'node:fs/promises';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

const projects = `
  [[test.projects]]
  name = "Chromium"
  dependencies = ["setup"]
  [[test.projects]]
  name = "setup"
  default = false
  [[test.projects]]
  name = "slow"
  default = false
  [[test.projects]]
  name = "slow-nightly"
  default = false
`;

async function selectedBy(args) {
  const root = await workspace({
    'mark.test.ts': `import { test } from '@ferridriver/test';
      import { writeFileSync } from 'node:fs';
      test('mark', async ({}, info) => { writeFileSync(info.project.name + '.ran', ''); });`,
    'ferridriver.toml': `[test]
      testMatch = ["*.test.ts"]
      reporter = [{ name = "null" }]
      ${projects}`,
  });
  const result = await run(['test', '--no-inherit', ...args], { cwd: root });
  const ran = (await readdir(root)).filter(file => file.endsWith('.ran')).map(file => file.slice(0, -4)).sort();
  return { result, ran };
}

test('a run without --project skips default: false projects but keeps their dependents\' setup', async () => {
  const { result, ran } = await selectedBy([]);
  passed(result);
  assert.deepEqual(ran, ['Chromium', 'setup']);
});

test('--project selects a default: false project by name', async () => {
  const { result, ran } = await selectedBy(['--project', 'slow']);
  passed(result);
  assert.deepEqual(ran, ['slow']);
});

test('--project matches case-insensitively and expands wildcards', async () => {
  const { result, ran } = await selectedBy(['--project', 'chromium', '--project', 'SLOW*']);
  passed(result);
  assert.deepEqual(ran, ['Chromium', 'setup', 'slow', 'slow-nightly']);
});

test('--project with an unknown name fails the run and names the projects', async () => {
  const { result, ran } = await selectedBy(['--project', 'firefox']);
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /Project\(s\) "firefox" not found\. Available projects: "Chromium", "setup", "slow", "slow-nightly"/);
  assert.deepEqual(ran, []);
});

test('--project with a wildcard that matches nothing fails the run', async () => {
  const { result, ran } = await selectedBy(['--project', 'webkit*']);
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /No projects matched\. Available projects:/);
  assert.deepEqual(ran, []);
});
