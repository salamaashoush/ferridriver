import assert from 'node:assert/strict';
import { readdir } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

for (const screenshot of [true, false]) {
  test(`a failing test honors screenshotOnFailure=${screenshot}`, async () => {
    const cwd = await workspace({
      'fail.spec.ts': `import { test, expect } from '@ferridriver/test';
        test('intentional title mismatch', async ({ page }) => {
          await page.setContent('<h1>x</h1>');
          expect(await page.title()).toBe('never');
        });`,
      'ferridriver.toml': `[test]
        testMatch = ["*.spec.ts"]
        workers = 1
        retries = 0
        screenshotOnFailure = ${screenshot}
        outputDir = "out"
        reporter = [{ name = "list" }]
        [test.browser]
        browser = "chromium"
        backend = "cdp-pipe"
        headless = true
      `,
    });
    const result = await run(['test', '--no-inherit'], { cwd });
    assert.equal(result.code, 1, result.text);
    assert.match(result.text, /never/, 'the intentional assertion must cause the failure');
    const files = await readdir(join(cwd, 'out'), { recursive: true }).catch(error => {
      if (error.code === 'ENOENT') return [];
      throw error;
    });
    assert.equal(files.some(file => file.endsWith('.png')), screenshot, result.text);
  });
}
