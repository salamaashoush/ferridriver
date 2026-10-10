import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

async function suite(source, env = {}) {
  const cwd = await workspace({
    'probe.spec.ts': source,
    'ferridriver.toml': `[test]
      testMatch = ["*.spec.ts"]
      workers = 1
      retries = 0
      timeout = 20000
      outputDir = "out"
      screenshotOnFailure = false
      reporter = [{ name = "list" }]
      [test.browser]
      browser = "chromium"
      backend = "cdp-pipe"
      headless = true
    `,
  });
  const result = await run(['test', '--no-inherit'], { cwd, env });
  const files = await readdir(join(cwd, 'out'), { recursive: true }).catch(error => {
    if (error.code === 'ENOENT') return [];
    throw error;
  });
  const contexts = {};
  for (const file of files.filter(f => f.endsWith('.md'))) {
    contexts[file.split('/')[0]] = await readFile(join(cwd, 'out', file), 'utf8');
  }
  return { result, contexts };
}

function only(contexts, title) {
  const key = Object.keys(contexts).find(k => k.includes(title));
  assert.ok(key, `no error context for "${title}": ${Object.keys(contexts).join(', ')}`);
  return contexts[key];
}

test('a web-first failure carries the tree it judged and the source around it', async () => {
  const { result, contexts } = await suite(`import { test, expect } from '@ferridriver/test';
test('mismatch', async ({ page }) => {
  await page.setContent('<button>Hi</button>');
  await expect(page.getByRole('button')).toHaveText('Bye', { timeout: 300 });
});
test('passes', async ({ page }) => {
  await page.setContent('<p>ok</p>');
});`);
  assert.equal(result.code, 1, result.text);
  assert.equal(Object.keys(contexts).length, 1, `only the failing test leaves a context: ${Object.keys(contexts)}`);
  const text = only(contexts, 'mismatch');
  assert.ok(text.startsWith('# Instructions\n\n- Following Playwright test failed.'), text);
  assert.ok(text.includes('- Name: probe.spec.ts >> mismatch'), text);
  assert.ok(text.includes('- Location: probe.spec.ts:2:'), text);
  assert.ok(text.includes('```\nexpect(locator).toHaveText(expected) failed\n'), text);
  assert.ok(text.includes('```yaml\n- button "Hi"\n```'), text);
  assert.ok(!text.includes('# Page snapshot'), 'the matcher snapshot replaces the page snapshot');
  assert.ok(text.includes("> 4 |   await expect(page.getByRole('button')).toHaveText('Bye', { timeout: 300 });"), text);
  assert.ok(text.includes(' ^ expect(locator).toHaveText(expected) failed'), text);
  assert.match(result.text, /Error Context: .*error-context-attempt1\.md/, result.text);
});

test('any other failure gets the page as the test left it', async () => {
  const { result, contexts } = await suite(`import { test } from '@ferridriver/test';
test('throws', async ({ page }) => {
  await page.setContent('<h1>Title</h1><button>Go</button>');
  throw new Error('boom');
});`);
  assert.equal(result.code, 1, result.text);
  const text = only(contexts, 'throws');
  assert.ok(text.includes('```\nboom\n```'), text);
  assert.ok(text.includes('# Page snapshot\n\n```yaml\n'), text);
  assert.match(text, /- heading "Title" \[level=1\] \[ref=e\d+\]/, text);
  assert.match(text, /- button "Go" \[ref=e\d+\]/, text);
  assert.ok(text.includes("> 4 |   throw new Error('boom');"), text);
});

test('a soft web-first failure brings its snapshot into the context', async () => {
  const { result, contexts } = await suite(`import { test, expect } from '@ferridriver/test';
test('soft', async ({ page }) => {
  await page.setContent('<input type="checkbox">');
  await expect.soft(page.getByRole('checkbox')).toBeChecked({ timeout: 300 });
});`);
  assert.equal(result.code, 1, result.text);
  const text = only(contexts, 'soft');
  assert.ok(text.includes('expect(locator).toBeChecked() failed'), text);
  assert.ok(text.includes('```yaml\n- checkbox\n```'), text);
  assert.ok(!text.includes('# Page snapshot'), text);
});

test('PLAYWRIGHT_NO_COPY_PROMPT keeps the context but skips the page snapshot', async () => {
  const { result, contexts } = await suite(`import { test } from '@ferridriver/test';
test('throws', async ({ page }) => {
  await page.setContent('<h1>Title</h1>');
  throw new Error('boom');
});`, { PLAYWRIGHT_NO_COPY_PROMPT: '1' });
  assert.equal(result.code, 1, result.text);
  const text = only(contexts, 'throws');
  assert.ok(text.includes('```\nboom\n```'), text);
  assert.ok(!text.includes('# Page snapshot'), text);
});

test('the snapshot survives a rethrow from a step', async () => {
  const { result, contexts } = await suite(`import { test, expect } from '@ferridriver/test';
test('stepped', async ({ page }) => {
  await page.setContent('<a href="#">Link</a>');
  await test.step('check', async () => {
    await expect(page.getByRole('link')).toHaveText('Other', { timeout: 300 });
  });
});`);
  assert.equal(result.code, 1, result.text);
  const text = only(contexts, 'stepped');
  assert.ok(text.includes('```yaml\n- link "Link"'), text);
});
