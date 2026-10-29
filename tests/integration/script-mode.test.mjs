import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

function config({ scriptMatch = ['check.ts'], testMatch, extensions, retries = 0 } = {}) {
  return `${extensions ? `[extensions]\npaths = ${JSON.stringify(extensions)}\n` : ''}[test]
scriptMatch = ${JSON.stringify(scriptMatch)}
${testMatch ? `testMatch = ${JSON.stringify(testMatch)}\n` : ''}workers = 1
retries = ${retries}
`;
}

async function runScripts(files, args = []) {
  const cwd = await workspace(files);
  const result = await run(['test', '--no-inherit', '--headless', '--reporter', 'stream=stream.jsonl', ...args], { cwd });
  const events = (await readFile(join(cwd, 'stream.jsonl'), 'utf8').catch(() => ''))
    .split('\n').filter(Boolean).map((line) => JSON.parse(line));
  return { ...result, cwd, events, finished: events.filter((event) => event.kind === 'test-finished') };
}

const page = (body) => `await page.setContent(${JSON.stringify(body)});\n`;

test('a plain script runs as one test with its fixtures and expect in scope', async () => {
  const result = await runScripts({
    'ferridriver.toml': config(),
    'check.ts': page('<button onclick="this.textContent=\'done\'">go</button>') + `
await page.getByRole('button', { name: 'go' }).click();
await expect(page.getByRole('button')).toHaveText('done');
expect(context.pages()).toContain(page);
expect(typeof browser.version()).toBe('string');
expect(typeof request.get).toBe('function');
`,
  });
  passed(result);
  assert.equal(result.finished.length, 1, result.text);
  assert.equal(result.finished[0].outcome.test_id.name, 'check');
  assert.equal(result.finished[0].outcome.test_id.file, 'check.ts');
  assert.equal(result.finished[0].outcome.status, 'passed', result.text);
});

test('every browser call of a script is a step of its test', async () => {
  const result = await runScripts({
    'ferridriver.toml': config(),
    'check.ts': page('<a href="#x">link</a>') + `await page.getByText('link').click();
await test.step('named part', async () => {
  await expect(page.getByText('link')).toBeVisible();
});
`,
  });
  passed(result);
  const titles = result.events.filter((event) => event.kind === 'step-finished').map((event) => event.title);
  assert.deepEqual(titles.filter((title) => !title.startsWith('Expect')).slice(0, 3), ['Set content', 'Click', 'named part'], titles.join(', '));
});

test('a failing script reports the line the user wrote', async () => {
  const result = await runScripts({
    'ferridriver.toml': config(),
    'check.ts': `// a comment first, so line 1 is not where anything happens
${page('<h1>Hello</h1>')}
await expect(page.locator('h1')).toHaveText('Bye', { timeout: 300 });
`,
  });
  assert.notEqual(result.code, 0, result.text);
  const error = result.finished[0].outcome.error;
  assert.match(error.stack, /check\.ts:4:\d+/, error.stack);
  assert.match(error.message, /Bye/);
});

test('an import in a script is bundled, and a thrown error maps through it', async () => {
  const result = await runScripts({
    'ferridriver.toml': config(),
    'helper.ts': `export function fail(): never {\n  throw new Error('from the helper');\n}\n`,
    'check.ts': `import { fail } from './helper.ts';\n\nfail();\n`,
  });
  assert.notEqual(result.code, 0, result.text);
  const { stack } = result.finished[0].outcome.error;
  assert.match(stack, /helper\.ts:2:\d+/, stack);
  assert.match(stack, /check\.ts:3:\d+/, stack);
});

test('each repetition evaluates the script afresh', async () => {
  const result = await runScripts({
    'ferridriver.toml': config(),
    'check.ts': `console.log('ran repetition ' + test.info().repeatEachIndex);\n`,
  }, ['--repeat-each', '3']);
  passed(result);
  const printed = result.finished.map((event) => event.outcome.stdout.trim()).sort();
  assert.deepEqual(printed, ['ran repetition 0', 'ran repetition 1', 'ran repetition 2'], result.text);
});

test('a retry runs the script again', async () => {
  const result = await runScripts({
    'ferridriver.toml': config({ retries: 1 }),
    'check.ts': `if (test.info().retry === 0) throw new Error('first attempt fails');\n`,
  });
  passed(result);
  assert.deepEqual(result.finished.map((event) => event.outcome.status), ['failed', 'passed'], result.text);
});

test('scripts and test files run side by side, and a file both name is a script', async () => {
  const result = await runScripts({
    'ferridriver.toml': config({ scriptMatch: ['*.script.ts'], testMatch: ['*.ts'] }),
    'home.script.ts': page('<p>hi</p>') + `await expect(page.locator('p')).toHaveText('hi');\n`,
    'spec.ts': `import { test, expect } from '@ferridriver/test';
test('a spec', async ({ page }) => { expect(await page.title()).toBe(''); });
test('no stale script globals', async () => { expect(globalThis.page).toBeUndefined(); });
`,
  });
  passed(result);
  assert.deepEqual(result.finished.map((event) => event.outcome.test_id.name).sort(), ['a spec', 'home', 'no stale script globals'], JSON.stringify(result.finished));
});

test('a script keeps the automatic fixtures and overrides of extensions', async () => {
  const result = await runScripts({
    'ferridriver.toml': config({ extensions: ['./ext.ts'] }),
    'ext.ts': `defineFixtures({
  marker: [async ({ page }, use) => { await page.setContent('<p id="m">auto</p>'); await use(undefined); }, { auto: true }],
});`,
    'check.ts': `await expect(page.locator('#m')).toHaveText('auto');\n`,
  });
  passed(result);
});

test('registering a test from inside a script is refused and says why', async () => {
  const result = await runScripts({
    'ferridriver.toml': config(),
    'check.ts': `test('nested', async () => {});\n`,
  });
  assert.notEqual(result.code, 0, result.text);
  const { message } = result.finished[0].outcome.error;
  assert.match(message, /test\("nested"\) was called while a test is running/, message);
  assert.match(message, /test\.step\(\)/, message);
});

test('a config with scripts alone discovers no test files', async () => {
  const result = await runScripts({
    'ferridriver.toml': config(),
    'check.ts': `expect(1).toBe(1);\n`,
    'other.test.ts': `throw new Error('not a test file of this run');\n`,
  });
  passed(result);
  assert.equal(result.finished.length, 1, result.text);
});

test('listing a script compiles it and runs none of it', async () => {
  const cwd = await workspace({
    'ferridriver.toml': config(),
    'check.ts': `throw new Error('ran while listing');\n`,
  });
  const result = await run(['test', '--no-inherit', '--list', '--format', 'json'], { cwd });
  passed(result);
  const listing = JSON.parse(result.stdout);
  assert.deepEqual(listing.tests.map((t) => [t.file, t.line, t.titlePath]), [['check.ts', 1, ['check.ts', 'check']]]);
});
