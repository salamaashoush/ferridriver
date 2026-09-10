import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

for (const modifier of ['test.setTimeout(1000)', 'testInfo.setTimeout(1000)', 'test.slow()', 'test.setTimeout(0)']) {
  test(`runtime deadline honors ${modifier} while the body is pending`, async () => {
    const cwd = await workspace({
      'ferridriver.toml': '[test]\ntestMatch = ["*.test.ts"]\ntimeout = 100\nworkers = 1\n',
      'deadline.test.ts': `import { test } from '@ferridriver/test';
test('extends its active deadline', async ({ testInfo }) => {
  ${modifier};
  await new Promise(resolve => setTimeout(resolve, 200));
});`,
    });
    const result = await run(['test', '--no-inherit', '--headless'], { cwd });
    passed(result);
    assert.ok(result.text.includes('1 passed'), result.text);
  });
}

test('runtime deadline shortening accounts for time already spent in the body', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.ts"]\ntimeout = 1000\nworkers = 1\n',
    'deadline.test.ts': `import { test } from '@ferridriver/test';
test('shortens its active deadline', async () => {
  await new Promise(resolve => setTimeout(resolve, 150));
  test.setTimeout(100);
  await new Promise(resolve => setTimeout(resolve, 20));
  console.log('incorrectly restarted the budget');
  await new Promise(() => {});
});`,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  assert.ok(result.text.includes('test timed out after 100ms'), result.text);
  assert.ok(!result.text.includes('incorrectly restarted the budget'), result.text);
});

test('runtime deadline still times out an unresolved body after an extension', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.ts"]\ntimeout = 100\nworkers = 1\n',
    'deadline.test.ts': `import { test } from '@ferridriver/test';
test('extends but still hangs', async () => {
  test.setTimeout(200);
  await new Promise(() => {});
});`,
  });
  const result = await run(['test', '--no-inherit', '--headless'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  assert.ok(result.text.includes('test timed out after 200ms'), result.text);
});
