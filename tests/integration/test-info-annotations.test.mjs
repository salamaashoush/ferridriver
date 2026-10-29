import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

// testInfo.annotations is Playwright's one array: what the test declared and
// what it recorded at run time are on it, and what the body pushes onto it
// reaches the report like annotate() does.
test('annotations pushed onto testInfo.annotations reach the report once each', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["annotated.test.ts"]\nworkers = 1\n',
    'annotated.test.ts': `import { test, expect } from '@ferridriver/test';
test('annotated', { annotation: { type: 'owner', description: 'checkout' } }, async () => {
  const info = test.info();
  expect(info.annotations).toBe(info.annotations);
  expect(info.annotations).toEqual([{ type: 'owner', description: 'checkout' }]);
  info.annotations.push({ type: 'issue', description: 'ACME-1' });
  info.annotate('build', 'b7');
  expect(info.annotations.map((a) => a.type)).toEqual(['owner', 'issue', 'build']);
});
`,
  });
  const result = await run(['test', '--no-inherit', '--headless', '--reporter', 'stream=stream.jsonl'], { cwd });
  passed(result);
  const finished = (await readFile(join(cwd, 'stream.jsonl'), 'utf8'))
    .split('\n').filter(Boolean).map((line) => JSON.parse(line))
    .find((event) => event.kind === 'test-finished');
  const infos = finished.outcome.annotations.filter((a) => a.info).map((a) => [a.info.type_name, a.info.description]);
  assert.deepEqual(infos.sort(), [['build', 'b7'], ['issue', 'ACME-1'], ['owner', 'checkout']], JSON.stringify(finished.outcome.annotations));
});
