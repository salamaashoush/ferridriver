import assert from 'node:assert/strict';
import { readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

// `--shuffle [seed]` (Playwright 1.64) schedules in an order the seed
// decides and reports the seed; `-G` (1.61) is `--grep-invert`.
async function suite() {
  return await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\nworkers = 1\nretries = 0\nfullyParallel = true\n',
    'order.test.mjs': `import { test } from '@ferridriver/test';
      import { appendFile } from 'node:fs/promises';
      for (const name of ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'])
        test(name, async () => { await appendFile('order.txt', name); });`,
  });
}

async function order(cwd, args) {
  await rm(join(cwd, 'order.txt'), { force: true });
  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', ...args], { cwd });
  assert.equal(result.code, 0, result.text);
  return { order: await readFile(join(cwd, 'order.txt'), 'utf8'), text: result.text };
}

test('a seed fixes the order and the header reports it', async () => {
  const cwd = await suite();
  const plain = await order(cwd, []);
  assert.equal(plain.order, 'abcdefgh');
  const first = await order(cwd, ['--shuffle', '271828182']);
  const second = await order(cwd, ['--shuffle', '271828182']);
  assert.equal(first.order, second.order);
  assert.notEqual(first.order, 'abcdefgh');
  assert.equal([...first.order].sort().join(''), 'abcdefgh');
  assert.match(first.text, /using 1 worker, shuffle seed 271828182/);
  const other = await order(cwd, ['--shuffle', '314159']);
  assert.notEqual(other.order, first.order);
});

test('a bare --shuffle picks a seed and reports it', async () => {
  const cwd = await suite();
  const shuffled = await order(cwd, ['--shuffle']);
  const seed = shuffled.text.match(/shuffle seed (\d+)/)?.[1];
  assert.ok(seed, shuffled.text);
  assert.equal((await order(cwd, ['--shuffle', seed])).order, shuffled.order);
});

test('-G skips tests matching the pattern', async () => {
  const cwd = await suite();
  assert.equal((await order(cwd, ['-G', '^.*[aeiou]$'])).order, 'bcdfgh');
});
