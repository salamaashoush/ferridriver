import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

for (const fixture of ['context', 'page']) {
  test(`${fixture} resolves browser startup as a separate fixture`, async () => {
    const cwd = await workspace({
      'ferridriver.toml': `[test]
testMatch = ["*.spec.ts"]
workers = 1
[test.browser]
headless = true
executablePath = "./missing-browser"
`,
      'startup.spec.ts': `import { test } from '@ferridriver/test';
test('requires a browser', async ({ ${fixture} }) => { void ${fixture}; });`,
    });
    const result = await run(['test', '--no-inherit', '--headless'], { cwd });
    assert.notEqual(result.code, 0, result.text);
    assert.match(result.text, /fixture 'browser' setup failed/);
  });
}
