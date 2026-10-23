import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

// Every toHaveScreenshot leaves a structured comparison on the attempt,
// beside the -expected, -actual and -diff attachments it names, so a
// reviewer (a report, a CI service) can tell a new screenshot from a
// changed one without parsing the error text.

const page = ({ box = 'red', clock = '10:00' } = {}) => `<style>
  body { margin: 0; background: white; font: 16px sans-serif; }
  #box { width: 120px; height: 80px; background: ${box}; }
  #clock { width: 120px; margin: 8px 0 0; }
</style><div id="box"></div><p id="clock">${clock}</p>`;

async function suite() {
  return workspace({
    'hero.spec.ts': `import { test, expect } from '@ferridriver/test';
import { readFileSync } from 'node:fs';
test('hero', async ({ page }) => {
  await page.setViewportSize({ width: 240, height: 160 });
  await page.setContent(readFileSync('page.html', 'utf8'));
  await expect(page).toHaveScreenshot('hero.png');
});`,
    'page.html': page(),
    'ferridriver.toml': `[test]
testMatch = ["*.spec.ts"]
workers = 1
retries = 0
outputDir = "out"
reporter = [{ name = "list" }]
[test.browser]
browser = "chromium"
backend = "cdp-pipe"
headless = true
[test.expect.toHaveScreenshot]
mask = ["#clock"]
`,
  });
}

async function comparison(cwd, args) {
  const result = await run(['test', '--no-inherit', '--reporter', 'stream=events.jsonl', ...args.flags], {
    cwd,
    env: { FERRIDRIVER_TEST__EXPECT__TO_HAVE_SCREENSHOT__PATH_TEMPLATE: `${cwd}/baselines/{testFilePath}/{arg}{-projectName}{ext}` },
  });
  const lines = (await readFile(join(cwd, 'events.jsonl'), 'utf8')).trim().split('\n').map(line => JSON.parse(line));
  const finished = lines.find(line => line.kind === 'test-finished');
  assert.ok(finished, result.text);
  const [snapshot, ...rest] = finished.outcome.snapshots ?? [];
  assert.ok(snapshot, `no snapshot comparison recorded\n${result.text}`);
  assert.equal(rest.length, 0);
  const attachment = name => finished.outcome.attachments.find(a => a.name === name);
  return { result, snapshot, attachment };
}

function onDisk(found, name) {
  const attached = found.attachment(name);
  assert.ok(attached, `${name} is not attached`);
  assert.ok(attached.path && existsSync(attached.path), `${name} is not on disk at ${attached.path}`);
  return attached.path;
}

test('a screenshot comparison is recorded as written, passed, changed and updated', async () => {
  const cwd = await suite();
  const baseline = join(cwd, 'baselines', 'hero.spec.ts', 'hero.png');

  const written = await comparison(cwd, { flags: ['--update-snapshots', 'missing'] });
  assert.equal(written.result.code, 0, written.result.text);
  assert.equal(written.snapshot.status, 'written');
  assert.equal(written.snapshot.kind, 'screenshot');
  assert.equal(written.snapshot.name, 'hero.png');
  assert.equal(written.snapshot.failed, false);
  assert.equal(written.snapshot.snapshot_path, baseline, 'an absolute path template places the baseline');
  assert.ok(existsSync(baseline));
  onDisk(written, written.snapshot.actual);

  const passed = await comparison(cwd, { flags: [] });
  assert.equal(passed.result.code, 0, passed.result.text);
  assert.equal(passed.snapshot.status, 'passed');
  assert.equal(passed.snapshot.diff_pixels, 0);
  assert.equal(passed.snapshot.width, 240);

  await writeFile(join(cwd, 'page.html'), page({ clock: '23:59' }));
  const masked = await comparison(cwd, { flags: [] });
  assert.equal(masked.result.code, 0, `a change under a config mask is not a difference\n${masked.result.text}`);
  assert.equal(masked.snapshot.status, 'passed');

  await writeFile(join(cwd, 'page.html'), page({ box: 'blue' }));
  const changed = await comparison(cwd, { flags: ['--reporter', 'html=report.html'] });
  assert.equal(changed.result.code, 1, changed.result.text);
  assert.equal(changed.snapshot.status, 'changed');
  assert.equal(changed.snapshot.failed, true);
  assert.ok(changed.snapshot.diff_ratio > 0, JSON.stringify(changed.snapshot));
  assert.ok(changed.snapshot.diff_pixels > 0);
  for (const name of [changed.snapshot.expected, changed.snapshot.actual, changed.snapshot.diff]) onDisk(changed, name);
  assert.deepEqual(
    [changed.snapshot.expected, changed.snapshot.actual, changed.snapshot.diff],
    ['hero-expected.png', 'hero-actual.png', 'hero-diff.png'],
  );
  const report = await readFile(join(cwd, 'report.html'), 'utf8');
  assert.match(report, /Visual comparisons/);
  assert.match(report, /"snapshots":\[\{"name":"hero.png"/);

  const updated = await comparison(cwd, { flags: ['--update-snapshots', 'changed'] });
  assert.equal(updated.result.code, 0, updated.result.text);
  assert.equal(updated.snapshot.status, 'updated');
  assert.equal(updated.snapshot.failed, false);
  assert.ok(updated.snapshot.diff_pixels > 0);
  const replaced = onDisk(updated, updated.snapshot.expected);
  const actual = onDisk(updated, updated.snapshot.actual);
  onDisk(updated, updated.snapshot.diff);
  assert.deepEqual(await readFile(baseline), await readFile(actual), 'the baseline is now the actual');
  assert.notDeepEqual(await readFile(replaced), await readFile(actual), 'the expected attachment is what was replaced');

  const after = await comparison(cwd, { flags: [] });
  assert.equal(after.snapshot.status, 'passed', after.result.text);
});
