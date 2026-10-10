import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

// `retryStrategy: 'isolated'` (Playwright 1.62) runs every retry after
// the rest of the suite, one at a time.
async function events(strategy) {
  const cwd = await workspace({
    'ferridriver.toml': `[test]\ntestMatch = ["*.test.mjs"]\nworkers = 3\nretries = 1\nfullyParallel = true\n${strategy ? `retryStrategy = "${strategy}"\n` : ''}`,
    'suite.test.mjs': `import { test, expect } from '@ferridriver/test';
      import { appendFile } from 'node:fs/promises';
      const mark = (kind, name) => appendFile('events.txt', kind + ':' + name + ':' + test.info().retry + '\\n');
      for (const name of ['flaky1', 'flaky2'])
        test(name, async () => {
          await mark('start', name);
          await new Promise(resolve => setTimeout(resolve, 150));
          await mark('end', name);
          expect(test.info().retry).toBe(1);
        });
      for (const name of ['slow1', 'slow2', 'slow3', 'slow4'])
        test(name, async () => {
          await mark('start', name);
          await new Promise(resolve => setTimeout(resolve, 400));
          await mark('end', name);
        });`,
  });
  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml'], { cwd });
  assert.equal(result.code, 0, result.text);
  assert.match(result.text, /2 flaky/);
  return (await readFile(join(cwd, 'events.txt'), 'utf8')).trim().split('\n').map(line => {
    const [kind, name, retry] = line.split(':');
    return { kind, name, retry: Number(retry) };
  });
}

test('isolated retries wait for the suite and run one at a time', async () => {
  const log = await events('isolated');
  const firstRetry = log.findIndex(event => event.retry === 1);
  assert.ok(firstRetry > 0);
  // Nothing from a first attempt happens once the retries begin.
  assert.deepEqual(log.slice(firstRetry).filter(event => event.retry === 0), []);
  // The retries do not overlap: each ends before the next starts.
  assert.deepEqual(log.slice(firstRetry).map(event => event.kind), ['start', 'end', 'start', 'end']);
});

test('immediate retries, the default, run while the suite is still going', async () => {
  const log = await events('');
  const firstRetry = log.findIndex(event => event.retry === 1);
  assert.ok(log.slice(firstRetry).some(event => event.retry === 0), JSON.stringify(log));
});
