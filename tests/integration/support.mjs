import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';

export const repo = process.cwd();
export const binary = resolve(process.env.FERRIDRIVER_BIN ?? join(repo, 'target/debug/ferridriver'));

export async function fixtureServer(body) {
  await commands.start('fixtures', { binary: join(repo, 'target/debug/ferridriver-fixtures') });
  try {
    const ready = await commands.waitForOutput('fixtures', '\n');
    const match = ready.match(/serving (http:\/\/127\.0\.0\.1:\d+)/);
    assert.ok(match, ready);
    await body(match[1]);
  } finally { await commands.stop('fixtures'); }
}

export async function workspace(files) {
  const root = await mkdtemp(join(tmpdir(), 'ferridriver-integration-'));
  for (const [name, body] of Object.entries(files)) {
    const path = join(root, name);
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, body);
  }
  return root;
}

export function quote(value) {
  return "'" + String(value).replaceAll("'", "'\\''") + "'";
}

export async function run(args, { cwd = repo, input, env = {}, executable = binary } = {}) {
  const started = performance.now();
  let command = ['env', ...Object.entries(env).map(([key, value]) => `${key}=${value}`), executable, ...args].map(quote).join(' ');
  if (input !== undefined) {
    const inputRoot = await workspace({ 'stdin': input });
    command += ` < ${quote(join(inputRoot, 'stdin'))}`;
  }
  const result = await commands.exec('probe', { command: `cd ${quote(cwd)} && exec ${command}` });
  assert.notEqual(result.exitCode, null, `command terminated by signal\n${result.stderr}`);
  return { code: result.exitCode, stdout: result.stdout, stderr: result.stderr,
    text: result.stdout + result.stderr, elapsed: performance.now() - started };
}

export function passed(result) {
  assert.equal(result.code, 0, result.text);
}

export async function script(source, files = {}, { module = false, ...options } = {}) {
  const cwd = await workspace(module ? { ...files, 'main.ts': source } : files);
  const input = module ? ['main.ts'] : ['-e', source];
  const result = await run(['run', '--no-inherit', '--json', ...input], { ...options, cwd });
  passed(result);
  const response = JSON.parse(result.stdout);
  assert.equal(response.status, 'ok', result.text);
  return response;
}

export async function scriptError(source) {
  const cwd = await workspace({});
  const result = await run(['run', '--no-inherit', '--json', '-e', source], { cwd });
  assert.notEqual(result.code, 0, result.text);
  const response = JSON.parse(result.stdout);
  assert.equal(response.status, 'error', result.text);
  return response.error;
}

export async function runtimeProbe(operations, files = {}, env = {}) {
  const cwd = await workspace(files);
  const cache = join(cwd, 'cache');
  const result = await run([], {
    cwd, executable: join(repo, 'target/debug/ferridriver-runtime-probe'),
    input: JSON.stringify(operations), env: { ...env, FERRIDRIVER_CACHE_DIR: cache },
  });
  passed(result);
  return { results: JSON.parse(result.stdout), cwd, cache };
}

export function observation(result) {
  assert.ok(!Object.hasOwn(result, 'error'), result.error);
  return result.value;
}
