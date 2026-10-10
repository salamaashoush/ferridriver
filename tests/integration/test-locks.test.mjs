import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { run, workspace } from './support.mjs';

// Test locks (Playwright 1.63): tests sharing a lock name never run at
// the same time, across files and workers; everything else stays parallel.
const body = `
  import { appendFile } from 'node:fs/promises';
  const span = async name => {
    await appendFile('events.txt', 'start:' + name + '\\n');
    await new Promise(resolve => setTimeout(resolve, 200));
    await appendFile('events.txt', 'end:' + name + '\\n');
  };`;

test('tests sharing a lock never overlap; unlocked ones still do', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["specs/*.test.mjs"]\nworkers = 4\nretries = 0\nfullyParallel = true\n',
    'specs/a.test.mjs': `import { test } from '@ferridriver/test';${body}
      test('a1', { lock: 'settings' }, () => span('L:a1'));
      test('a2', { lock: ['settings', 'other'] }, () => span('L:a2'));
      test('free1', () => span('F:free1'));
      test('free2', () => span('F:free2'));`,
    'specs/b.test.mjs': `import { test } from '@ferridriver/test';${body}
      test.describe('group', { lock: 'settings' }, () => {
        test('b1', () => span('L:b1'));
      });
      test('free3', () => span('F:free3'));`,
    'specs/c.test.mjs': `import { test } from '@ferridriver/test';${body}
      test.describe.configure({ lock: 'settings' });
      test('c1', () => span('L:c1'));`,
  });
  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', '--reporter', 'json'], { cwd });
  assert.equal(result.code, 0, result.text);
  const log = (await readFile(join(cwd, 'events.txt'), 'utf8')).trim().split('\n');
  let locked = 0, free = 0, maxLocked = 0, maxFree = 0;
  for (const line of log) {
    const [kind, name] = line.split(/:(.*)/s);
    const delta = kind === 'start' ? 1 : -1;
    if (name.startsWith('L:')) maxLocked = Math.max(maxLocked, locked += delta);
    else maxFree = Math.max(maxFree, free += delta);
  }
  assert.equal(log.filter(line => line.startsWith('start:L:')).length, 4, log.join(','));
  assert.equal(maxLocked, 1, log.join(','));
  assert.ok(maxFree > 1, log.join(','));
  // A lock schedules a test; it is not one of its reported annotations.
  assert.doesNotMatch(result.text, /"type":\s*"lock"/);
});
