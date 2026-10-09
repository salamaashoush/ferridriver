import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

const spec = `import { test, describe } from '@ferridriver/test';

describe('described fixme', () => {
  test.fixme('fixme description');
  test('under a described fixme', () => {});
});

test('runtime skip with only a description', () => {
  test.skip('skip description');
  throw new Error('unreachable');
});

test('runtime conditional skip', () => {
  test.skip(true, 'conditional description');
});

test('runtime fail with only a description', () => {
  test.fail('fail description');
  throw new Error('expected to fail');
});

test('runtime slow with only a description', () => {
  test.slow('slow description');
});

test('runtime skip with a falsy condition', () => {
  test.skip(false, 'never recorded');
});
`;

async function report() {
  const root = await workspace({
    'modifiers.test.ts': spec,
    'ferridriver.toml': `[test]
      testMatch = ["*.test.ts"]
      reporter = [{ name = "json", outputFile = "report.json" }]`,
  });
  passed(await run(['test', '--no-inherit'], { cwd: root }));
  const json = JSON.parse(await readFile(join(root, 'report.json'), 'utf8'));
  const specs = [];
  const walk = suite => {
    specs.push(...(suite.specs ?? []));
    (suite.suites ?? []).forEach(walk);
  };
  json.suites.forEach(walk);
  return Object.fromEntries(specs.map(s => [s.title, s.tests[0]]));
}

test('a description-only modifier records its description, at collection and at runtime', async () => {
  const tests = await report();
  const annotations = title => tests[title].annotations.map(({ type, description }) => ({ type, description }));
  const status = title => tests[title].results.at(-1).status;

  assert.deepEqual(annotations('under a described fixme'), [{ type: 'fixme', description: 'fixme description' }]);
  assert.equal(status('under a described fixme'), 'skipped');

  assert.deepEqual(annotations('runtime skip with only a description'), [{ type: 'skip', description: 'skip description' }]);
  assert.equal(status('runtime skip with only a description'), 'skipped');

  assert.deepEqual(annotations('runtime conditional skip'), [{ type: 'skip', description: 'conditional description' }]);

  assert.deepEqual(annotations('runtime fail with only a description'), [{ type: 'fail', description: 'fail description' }]);
  assert.equal(tests['runtime fail with only a description'].expectedStatus, 'failed');
  assert.equal(status('runtime fail with only a description'), 'failed');

  assert.deepEqual(annotations('runtime slow with only a description'), [{ type: 'slow', description: 'slow description' }]);

  assert.deepEqual(annotations('runtime skip with a falsy condition'), []);
  assert.equal(status('runtime skip with a falsy condition'), 'passed');
});
