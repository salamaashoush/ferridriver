import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

function success(result) {
  const run = observation(result);
  assert.equal(run.status, 'ok', JSON.stringify(run));
  return run;
}
async function execute(source, { args = [], files = {}, setup = [] } = {}) {
  const { results } = await runtimeProbe([...setup, { op: 'run-engine', source, args }], files);
  for (const result of results.slice(0, -1)) observation(result);
  return success(results.at(-1));
}

for (const [title, source, value] of [
  ['evaluates expressions', 'return 1 + 2', 3],
  ['serializes nested values and Unicode', "return { a: 1, b: [2, 3, { c: 'nested', d: [true, null] }], unicode: 'héllo \\u{1f680}' };", { a: 1, b: [2, 3, { c: 'nested', d: [true, null] }], unicode: 'héllo \u{1f680}' }],
  ['leaves artifacts undefined when not provided', 'return typeof artifacts', 'undefined'],
  ['exposes the runtime and framework module identities', `const fd = await import('ferridriver'); const cukes = await import('@cucumber/cucumber');
    return { host: ferridriver.host, same: fd.ferridriver === ferridriver, tool: typeof fd.tool,
      tools: fd.tools === ferridriver.tools, noPublicPlugins: typeof fd.plugins,
      noGlobalPlugins: typeof globalThis.plugins, bdd: typeof ferridriver.bdd.Given,
      cucumber: cukes.Given === ferridriver.bdd.Given };`,
    { host: 'script', same: true, tool: 'function', tools: true, noPublicPlugins: 'undefined', noGlobalPlugins: 'undefined', bdd: 'function', cucumber: true }],
]) {
  test(`script engine ${title}`, async () => assert.deepEqual((await execute(source)).value, value));
}

test('script engine binds hostile argument strings without interpolation', async () => {
  const payload = "'; drop table users; --";
  assert.equal((await execute('return args[0]', { args: [payload] })).value, payload);
});

test('script engine binds structured arguments without losing their types', async () => {
  const args = ['plain string', { user: { name: 'sashoush', tags: ['a', 'b'] } }, [1, 2, 3, null, false]];
  const result = await execute('return { s: args[0], obj: args[1], arr: args[2] }', { args });
  assert.deepEqual(result.value, { s: args[0], obj: args[1], arr: args[2] });
});

test('script engine captures console messages and formats multiple arguments', async () => {
  const result = await execute("console.log('hello'); console.warn('be careful', 42); return true");
  assert.equal(result.console.length, 2);
  assert.equal(result.console[0].message, 'hello');
  assert.ok(result.console[1].message.includes('be careful'));
  assert.ok(result.console[1].message.includes('42'));
});

test('script engine preserves all console levels in order', async () => {
  const result = await execute("console.log('log-msg'); console.info('info-msg'); console.warn('warn-msg'); console.error('error-msg'); console.debug('debug-msg'); return null;");
  assert.deepEqual(result.console.map(entry => entry.level), ['log', 'info', 'warn', 'error', 'debug']);
});

test('script engine exposes configured commands to plain scripts', async () => {
  const result = await execute("return await ferridriver.commands.run('echoValue', { value: 'hello' })", {
    setup: [{ op: 'configure-context', commands: { echoValue: { run: ['echo', '${value}'], output: 'text' } } }],
  });
  assert.equal(result.value, 'hello');
});

test('script variables persist in the host store after execution', async () => {
  const { results } = await runtimeProbe([
    { op: 'run-engine', source: "vars.set('greeting', 'hi'); return vars.get('greeting')" },
    { op: 'read-var', name: 'greeting' },
  ]);
  assert.equal(success(results[0]).value, 'hi');
  assert.equal(observation(results[1]), 'hi');
});

test('script engine reads its writes through both filesystem APIs', async () => {
  const result = await execute(`await fs.promises.writeFile('note.txt', 'hello world');
    return { viaPromise: await fs.promises.readFile('note.txt', 'utf8'),
      viaSync: fs.readFileSync('note.txt', 'utf8'), length: fs.readFileSync('note.txt').length };`);
  assert.deepEqual(result.value, { viaPromise: 'hello world', viaSync: 'hello world', length: 11 });
});

test('script filesystem paths can traverse a parent component', async () => {
  const result = await execute("return fs.readFileSync('nested/../outside.txt', 'utf8')", {
    files: { 'nested/seed': '', 'outside.txt': 'reachable' },
    setup: [{ op: 'configure-context', scriptRoot: 'nested' }],
  });
  assert.equal(result.value, 'reachable');
});

for (const [title, source, kind, timeoutMs] of [
  ['reports syntax failures structurally', 'this is not js at all', 'syntax', undefined],
  ['interrupts an infinite loop at its deadline', 'while (true) {}', 'timeout', 150],
]) {
  test(`script engine ${title}`, async () => {
    const { results } = await runtimeProbe([{ op: 'run-engine', source, timeoutMs }]);
    const result = observation(results[0]);
    assert.equal(result.status, 'error');
    assert.equal(result.error.kind, kind);
    if (kind === 'syntax') assert.ok(result.error.message.length > 0);
  });
}

for (const [title, source, files, value, setup] of [
  ['relative imports', "const m = await import('./helper.js'); return m.greet('sashoush')", { 'helper.js': 'export function greet(name) { return `hi ${name}`; }' }, 'hi sashoush', []],
  ['parent imports', "const m = await import('../shared.js'); return m.shared", { 'nested/seed': '', 'shared.js': "export const shared = 'from above';" }, 'from above', [{ op: 'configure-context', scriptRoot: 'nested' }]],
  ['nested imports', "const m = await import('./lib/util/math.js'); return m.double(21)", { 'lib/util/math.js': 'export const double = n => n * 2;' }, 42, []],
]) {
  test(`script engine resolves ${title}`, async () => assert.equal((await execute(source, { files, setup })).value, value));
}

test('script filesystem lists directory contents', async () => {
  const result = await execute("const entries = await fs.promises.readdir('files'); return entries.sort()", {
    files: { 'files/a.txt': 'x', 'files/b.txt': 'y', 'files/sub/seed': '' },
  });
  assert.deepEqual(result.value, ['a.txt', 'b.txt', 'sub']);
});

test('script filesystem distinguishes present and missing files', async () => {
  const result = await execute("return { has: fs.existsSync('present.txt'), missing: fs.existsSync('nothing.txt') }", { files: { 'present.txt': 'x' } });
  assert.deepEqual(result.value, { has: true, missing: false });
});

test('script runtime errors retain their message and source snippet when a line is known', async () => {
  const { results } = await runtimeProbe([{ op: 'run-engine', source: "\nlet x = 1;\nlet y = 2;\nthrow new Error('deliberate');\nreturn x + y;" }]);
  const result = observation(results[0]);
  assert.equal(result.status, 'error');
  assert.equal(result.error.kind, 'runtime');
  assert.ok(result.error.message.includes('deliberate'));
  if (result.error.line != null) assert.notEqual(result.error.sourceSnippet ?? result.error.source_snippet, undefined);
});

test('script engine rejects unresolved bare imports', async () => {
  const result = await execute("try { await import('lodash'); return 'no-error'; } catch (e) { return 'rejected: ' + String(e).slice(0, 30); }");
  assert.ok(result.value.startsWith('rejected'));
});

test('script artifacts round trip text and bytes and remove only the selected file', async () => {
  const { results, cwd } = await runtimeProbe([
    { op: 'configure-context', artifacts: 'artifacts' },
    { op: 'run-engine', source: `await artifacts.write('note.txt', 'hello');
      await artifacts.writeBytes('bin.dat', [1, 2, 3, 255]);
      const got = await artifacts.read('note.txt'); const bytes = await artifacts.readBytes('bin.dat');
      const entries = (await artifacts.list()).sort(); const removed = await artifacts.remove('note.txt');
      return { got, bytes: Array.from(bytes), entries, removed, afterRemove: await artifacts.exists('note.txt') };` },
  ]);
  observation(results[0]);
  assert.deepEqual(success(results[1]).value, { got: 'hello', bytes: [1, 2, 3, 255], entries: ['bin.dat', 'note.txt'], removed: true, afterRemove: false });
  assert.deepEqual(Array.from(await readFile(join(cwd, 'artifacts/bin.dat'))), [1, 2, 3, 255]);
});

test('script artifacts anchor relative names at their configured root', async () => {
  const { results, cwd } = await runtimeProbe([
    { op: 'configure-context', artifacts: 'artifacts' },
    { op: 'run-engine', source: "await artifacts.write('nested/out.txt', 'x'); return 'written'" },
  ]);
  observation(results[0]);
  assert.equal(success(results[1]).value, 'written');
  assert.equal(await readFile(join(cwd, 'artifacts/nested/out.txt'), 'utf8'), 'x');
});

test('fresh engine runs isolate globals even when they share the host context', async () => {
  const { results } = await runtimeProbe([
    { op: 'run-engine', source: 'globalThis.leak = 42; return 1' },
    { op: 'run-engine', source: 'return typeof globalThis.leak' },
  ]);
  assert.equal(success(results[0]).value, 1);
  assert.equal(success(results[1]).value, 'undefined');
});
