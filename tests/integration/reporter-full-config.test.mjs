import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

// FullConfig.filteredProjects (Playwright 1.64), argv and failOnFlakyTests
// (1.61), as a reporter's onBegin sees them.
test('a reporter reads the selected projects, argv and failOnFlakyTests', async () => {
  const cwd = await workspace({
    'ferridriver.toml': `[test]
      testMatch = ["*.test.mjs"]
      workers = 1
      failOnFlakyTests = true
      reporter = [{ name = "./reporter.mjs" }]
      [[test.projects]]
      name = "one"
      [[test.projects]]
      name = "two"
    `,
    'reporter.mjs': `import { writeFileSync } from 'node:fs';
      export default class Reporter {
        onBegin(config) {
          writeFileSync('config.json', JSON.stringify({
            projects: config.projects.map(p => p.name),
            filtered: config.filteredProjects.map(p => p.name),
            argv: config.argv,
            failOnFlakyTests: config.failOnFlakyTests,
          }));
        }
      }`,
    'a.test.mjs': `import { test } from '@ferridriver/test';
      test('passes', () => {});`,
  });
  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', '--project', 'two'], { cwd });
  assert.equal(result.code, 0, result.text);
  const seen = JSON.parse(await readFile(join(cwd, 'config.json'), 'utf8'));
  assert.deepEqual(seen.projects, ['one', 'two']);
  assert.deepEqual(seen.filtered, ['two']);
  assert.equal(seen.failOnFlakyTests, true);
  assert.deepEqual(seen.argv.slice(-2), ['--project', 'two']);
});
