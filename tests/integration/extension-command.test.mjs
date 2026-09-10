import assert from 'node:assert/strict';
import { chmod, mkdir, readFile, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, quote, run, workspace } from './support.mjs';

async function project(requires) {
  return workspace({
    'pkg/package.json': JSON.stringify({ name: '@acme/ext', type: 'module', ferridriver: { entries: ['./src/tool.ts'], requires } }),
    'pkg/src/lib/shared.ts': "export const NAME = 'acme.tool';",
    'pkg/src/tool.ts': "import { NAME } from './lib/shared'; defineTool({ name: NAME, description: 'p', exposeAsTool: true, handler: async () => ({ ok: true }) });",
  });
}
async function checker(cwd, output = '', code = 0) {
  const path = join(cwd, 'stub-tsc');
  await writeFile(join(cwd, 'stub-stdout.txt'), output);
  await writeFile(path, `#!/bin/sh\nprintf '%s\\n' "$@" > ${quote(join(cwd, 'argv.txt'))}\nfor a in "$@"; do case "$a" in *tsconfig.json) cp "$a" ${quote(join(cwd, 'tsconfig.copy.json'))};; esac; done\ncat ${quote(join(cwd, 'stub-stdout.txt'))}\nexit ${code}\n`);
  await chmod(path, 0o755);
  return { FERRIDRIVER_TSC: path };
}
const check = (cwd, env, extra = []) => run(['ext', 'check', './pkg', '--no-inherit', ...extra], { cwd, env });

test('extension checks report declared entries and promoted tools and typecheck the entry with native declarations', async () => {
  const cwd = await project();
  const result = await check(cwd, await checker(cwd));
  passed(result);
  assert.ok(result.stdout.includes('1 declared entry/entries'));
  assert.ok(result.stdout.split('\n').find(line => line.includes('acme.tool'))?.includes('mcp tool'));
  assert.ok(!result.stdout.includes('shared.ts'));
  const argv = await readFile(join(cwd, 'argv.txt'), 'utf8');
  assert.ok(argv.includes('--noEmit'));
  assert.ok(argv.split('\n').some(line => line.endsWith('tsconfig.json')));
  const config = await readFile(join(cwd, 'tsconfig.copy.json'), 'utf8');
  assert.ok(config.includes('src/tool.ts'));
  assert.ok(config.includes('@ferridriver/extension/index.d.ts'));
  assert.ok(config.includes(join(cwd, 'pkg/tsconfig.json')) || !config.includes('extends'));
});

test('extension type errors fail the command and remain visible beside loaded tool registrations', async () => {
  const cwd = await project();
  const result = await check(cwd, await checker(cwd, "src/tool.ts(2,10): error TS2339: Property 'put' does not exist on type 'Vars'.", 1));
  assert.notEqual(result.code, 0);
  for (const text of ['error TS2339', 'failed', 'acme.tool']) assert.ok(result.stdout.includes(text), result.text);
});

test('the explicit no-typecheck option reports the omission and never invokes the compiler', async () => {
  const cwd = await project();
  const result = await check(cwd, await checker(cwd, 'error: should not run', 1), ['--no-typecheck']);
  passed(result);
  assert.ok(result.stdout.includes('skipped: --no-typecheck'));
  assert.equal(existsSync(join(cwd, 'argv.txt')), false);
});

test('an absent compiler is reported with installation instructions instead of claiming type verification', async () => {
  const cwd = await project();
  const path = join(cwd, 'empty-bin');
  await mkdir(path);
  const result = await check(cwd, { PATH: path }, ['--json']);
  const report = JSON.parse(result.stdout);
  for (const text of ['no TypeScript compiler available', 'typescript', 'FERRIDRIVER_TS_DOWNLOAD']) assert.ok(report.typecheck.skipped.includes(text), result.text);
  assert.equal(report.typecheck.passed, true);
  assert.equal(report.ok, true);
});

test('finding a package runner does not authorize downloading a compiler', async () => {
  const cwd = await project();
  const path = join(cwd, 'fake-bin');
  await mkdir(path);
  const npx = join(path, 'npx');
  await writeFile(npx, `#!/bin/sh\necho called >> ${quote(join(cwd, 'npx-called.txt'))}\nexit 0\n`);
  await chmod(npx, 0o755);
  await check(cwd, { PATH: path }, ['--json']);
  assert.equal(existsSync(join(cwd, 'npx-called.txt')), false);
});

test('unmet extension requirements fail the check and explain which command is missing', async () => {
  const cwd = await project({ commands: ['definitely-not-a-real-binary-xyz'] });
  const result = await check(cwd, await checker(cwd));
  assert.notEqual(result.code, 0);
  for (const text of ['unmet:', 'definitely-not-a-real-binary-xyz', 'skipped:']) assert.ok(result.stdout.includes(text), result.text);
});

test('ext types generates resolvable declaration packages for extensions and tests', async () => {
  const cwd = await workspace({});
  const out = join(cwd, 'node_modules');
  passed(await run(['ext', 'types', '--out', out, '--no-inherit'], { cwd }));
  for (const name of ['@ferridriver/extension', '@ferridriver/test']) {
    const declarations = await readFile(join(out, name, 'index.d.ts'), 'utf8');
    assert.ok(declarations.length > 0);
    const manifest = await readFile(join(out, name, 'package.json'), 'utf8');
    assert.ok(manifest.includes(name));
    assert.ok(manifest.includes('index.d.ts'));
  }
  const declaration = await readFile(join(out, '@ferridriver/extension/index.d.ts'), 'utf8');
  assert.ok(declaration.includes('function defineTool'));
  assert.ok(declaration.includes('interface ToolContext'));
});
