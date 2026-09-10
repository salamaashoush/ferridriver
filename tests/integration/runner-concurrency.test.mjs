import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

test('the gate runtime keeps retry timers responsive beside busy script realms', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[test]\ntestMatch = ["*.test.mjs"]\nworkers = 3\ntimeout = 5000\nretries = 0\n',
    'contention.test.mjs': `
      import { test, expect } from '@ferridriver/test';
      import { readFile, writeFile } from 'node:fs/promises';
      test('retry timer', async () => {
        const attempts = [];
        const started = Date.now();
        let failure = '';
        try {
          await expect(async () => {
            attempts.push(Date.now() - started);
            if (attempts.length === 1) await writeFile('ready', 'ready');
            throw new Error('retry me');
          }).toPass({ timeout: 400, intervals: [50] });
        } catch (error) { failure = error.message; }
        expect(failure).toContain('retry me');
        expect(attempts.length, JSON.stringify(attempts)).toBeGreaterThanOrEqual(5);
      });
      for (let index = 0; index < 2; index++) {
        test('busy realm ' + index, async () => {
          await expect(async () => { await readFile('ready'); }).toPass({ intervals: [1], timeout: 5000 });
          const started = Date.now();
          while (Date.now() - started < 350) {}
        });
      }
    `,
  });
  const result = await run(['test', '--no-inherit', '--config', 'ferridriver.toml', '--headless'], { cwd });
  passed(result);
  assert.match(result.text, /3 passed/);
});
