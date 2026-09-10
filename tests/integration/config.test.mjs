import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

test('config truncates a long Unicode value without splitting a character', async () => {
  const cwd = await workspace({
    'ferridriver.toml': `[mcp.server]\ninstructions = ${JSON.stringify('→ ferridriver → drive the browser → '.repeat(12))}\n`,
  });
  const result = await run(['config', '--no-inherit'], { cwd });
  passed(result);
  assert.match(result.stdout, /mcp.server.instructions/);
  assert.match(result.stdout, /chars\)/);
});

test('doctor inspects sandbox roots without creating them', async () => {
  const cwd = await workspace({ 'ferridriver.toml': 'scriptRoot = "./scripts"\nartifactsRoot = "./out"\n' });
  const result = await run(['doctor', '--no-inherit'], { cwd });
  assert.match(result.stdout, /sandbox roots/);
  for (const path of ['scripts', 'out']) assert.equal(existsSync(join(cwd, path)), false, result.text);
});

test('doctor rejects two instances sharing a profile and explains the fix', async () => {
  const cwd = await workspace({
    'ferridriver.toml': `[mcp.browser]
      userDataDir = "/tmp/ferridriver-shared-profile"
      [mcp.browser.instances.staging]
      args = []
      [mcp.browser.instances.dev]
      args = []
    `,
  });
  const result = await run(['doctor', '--instances', '--no-inherit'], { cwd });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.stdout, /both launch with profile/);
  assert.ok(result.stdout.includes('${INSTANCE}'), result.text);
});

test('instance placeholders resolve to distinct profile directories', async () => {
  const cwd = await workspace({});
  await writeFile(join(cwd, 'ferridriver.toml'), `[mcp.browser]
    userDataDir = ${JSON.stringify(join(cwd, 'profiles', '${INSTANCE}'))}
    [mcp.browser.instances.staging]
    args = []
    [mcp.browser.instances.dev]
    args = []
  `);
  const result = await run(['doctor', '--instances', '--no-inherit'], { cwd });
  assert.doesNotMatch(result.stdout, /both launch with profile/);
  for (const name of ['staging', 'dev']) assert.ok(result.stdout.includes(join(cwd, 'profiles', name)), result.text);
});

test('an appended array reports every contributing configuration layer', async () => {
  const cwd = await workspace({
    'base.toml': '[mcp.browser]\nchromeArgs = ["--from-base"]\n',
    'ferridriver.toml': 'extends = "./base.toml"\n[mcp.browser]\nchromeArgs = ["--from-project"]\n',
  });
  const result = await run(['config', '--no-inherit'], { cwd });
  passed(result);
  const line = result.stdout.split('\n').find(line => line.includes('mcp.browser.chromeArgs'));
  assert.ok(line, result.text);
  assert.match(line, /base.toml/);
  assert.match(line, /ferridriver.toml/);
});

test('config names a shadowed configuration file', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\nworkers = 3\n',
    'ferridriver.yaml': 'test:\n  workers: 9\n',
  });
  const result = await run(['config', '--no-inherit'], { cwd });
  assert.match(result.stdout, /also present and ignored/);
  assert.match(result.stdout, /ferridriver.yaml/);
});
