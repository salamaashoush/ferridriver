import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { script } from './support.mjs';

test('script environments default to no variables while exposing inert identity', async () => {
  const { value } = await script(`return { keys: Object.keys(process.env), os: typeof process.platform,
    arch: typeof process.arch, ver: process.version, hasNode: 'node' in process.versions };`);
  assert.deepEqual(value.keys, []);
  assert.equal(value.os, 'string');
  assert.equal(value.arch, 'string');
  assert.ok(value.ver.startsWith('v'));
  assert.equal(value.hasNode, false);
});

test('script environments expose only declared variables that actually exist', async () => {
  const { value } = await script(`return { allowed: typeof process.env.PATH,
    allowedLen: (process.env.PATH ?? '').length > 0, undeclared: process.env.HOME ?? null,
    missing: process.env.FERRI_DEFINITELY_ABSENT_VAR_xyz ?? null };`, {
    'ferridriver.toml': '[scripting]\nallowEnv = ["PATH", "FERRI_DEFINITELY_ABSENT_VAR_xyz"]\n',
  });
  assert.deepEqual(value, { allowed: 'string', allowedLen: true, undeclared: null, missing: null });
});

test('process.exit throws instead of terminating its script host', async () => {
  const { value } = await script(`try { process.exit(2); return 'no throw'; } catch (error) { return String(error); }`);
  assert.match(value, /process\.exit\(2\) is not available/);
});

test('scripts cannot modify the frozen environment', async () => {
  const { value } = await script(`try { process.env.X = 'y'; } catch {} return process.env.X ?? 'still-unset';`);
  assert.equal(value, 'still-unset');
});

test('stdout and stderr writes are captured with their levels and trimmed newline', async () => {
  const response = await script(`const a = process.stdout.write('hello\\n');
    const b = process.stderr.write('boom'); return { a, b, tty: process.stdout.isTTY };`);
  assert.deepEqual(response.value, { a: true, b: true, tty: false });
  assert.ok(response.console.some(entry => entry.level === 'log' && entry.message === 'hello'));
  assert.ok(response.console.some(entry => entry.level === 'error' && entry.message === 'boom'));
});

test('hrtime provides tuple differences and a monotonic bigint clock', async () => {
  const { value } = await script(`const start = process.hrtime();
    const diff = process.hrtime(start);
    const first = process.hrtime.bigint(); const second = process.hrtime.bigint();
    return { tuple: Array.isArray(start) && start.length === 2,
      diffOk: diff[0] >= 0 && diff[1] >= 0, bigintType: typeof process.hrtime.bigint(), monotonic: second >= first };`);
  assert.deepEqual(value, { tuple: true, diffOk: true, bigintType: 'bigint', monotonic: true });
});

test('nextTick and promise callbacks follow microtask FIFO order', async () => {
  const { value } = await script(`const order = [];
    process.nextTick(() => order.push('nexttick'));
    Promise.resolve().then(() => order.push('promise'));
    await Promise.resolve(); await Promise.resolve(); return order;`);
  assert.deepEqual(value, ['nexttick', 'promise']);
});
