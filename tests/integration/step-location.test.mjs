import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { listZipEntries, readZipEntry } from '../e2e/helpers/unzip.ts';
import { observation, passed, run, runtimeProbe, workspace } from './support.mjs';

test('step file locations and skip annotations survive blob reporting and HTML merging', async () => {
  const cwd = await workspace({
    'specs/steps.spec.ts': `import { test, expect } from '@ferridriver/test';
      test('reports where its steps happened', async ({ page }) => {
        await test.step('Given the checkout page', async () => {
          await page.setContent('<h1>checkout</h1>');
        }, { location: { file: 'features/checkout.feature', line: 12, column: 3 } });
        await test.step.skip('Then it charges the card', async () => {
          throw new Error('never runs');
        });
        await expect(page.locator('h1')).toHaveText('checkout');
      });`,
    'ferridriver.toml': `[test]
      testDir = "specs"
      testMatch = ["**/*.spec.ts"]
      workers = 1
      retries = 0
      reporter = [{ name = "blob" }]
      [test.browser]
      browser = "chromium"
      backend = "cdp-pipe"
      headless = true
    `,
  });
  const config = ['--no-inherit', '-c', join(cwd, 'ferridriver.toml')];
  passed(await run(['test', ...config], { cwd }));
  const zip = new Uint8Array(await readFile(join(cwd, 'test-results/report.zip')));
  const lines = listZipEntries(zip).filter(entry => entry.name.toLowerCase().endsWith('.jsonl'))
    .flatMap(entry => new TextDecoder().decode(readZipEntry(zip, entry)).split('\n').filter(line => line.trim()).map(JSON.parse));
  const started = lines.find(line => line.kind === 'step-started' && line.title === 'Given the checkout page');
  assert.ok(started, JSON.stringify(lines));
  assert.deepEqual(started.location, { file: 'features/checkout.feature', line: 12, column: 3 });
  const finished = lines.find(line => line.kind === 'test-finished');
  assert.ok(finished);
  const steps = finished.outcome.steps;
  const located = steps.find(step => step.title === 'Given the checkout page');
  assert.ok(located);
  assert.equal(located.location.file, 'features/checkout.feature');
  const skipped = steps.find(step => step.title === 'Then it charges the card');
  assert.ok(skipped);
  assert.equal(skipped.status, 'skipped');
  assert.equal(skipped.annotations[0].info.type_name, 'skip');
  assert.equal(lines.find(line => line.kind === 'header')?.schema, 4);
  passed(await run(['merge-reports', join(cwd, 'test-results'), '--reporter', 'html',
    '--output-dir', join(cwd, 'merged'), ...config], { cwd }));
  const html = await readFile(join(cwd, 'merged/report.html'), 'utf8');
  assert.ok(html.includes('features/checkout.feature:12:3'));
});

test('legacy and structured blob step locations retain their source coordinates', async () => {
  const { results } = await runtimeProbe([{ op: 'reporter-api',
    action: 'stepLocations', locations: ['features/legacy.feature:7', { file: 'spec.ts', line: 4, column: 2 }],
  }]);
  const [legacy, current] = observation(results[0]);
  assert.equal(legacy.file, 'features/legacy.feature');
  assert.equal(legacy.line, 7);
  assert.deepEqual(current, { file: 'spec.ts', line: 4, column: 2 });
});
