// A spec starts programs as a Playwright spec does under Node: a fixture
// that seeds a database or runs a CLI calls `node:child_process`. The
// realm grants every program by default ([scripting.permissions] narrows
// it), and a child sees the environment the spec passes it.

import { describe, expect, test } from '@ferridriver/test';
import { exec, execFileSync, execSync, spawn } from 'node:child_process';
import { promisify } from 'node:util';

const windows = process.platform === 'win32';

describe('node:child_process in a spec', () => {
  test('execSync runs a command through the shell and returns its output', () => {
    expect(execSync('echo ran').toString().trim()).toBe('ran');
  });

  test('execFileSync passes the env it is given and throws with the status on failure', () => {
    const [shell, flag, read] = windows ? ['cmd.exe', '/c', 'echo %ONLY%'] : ['sh', '-c', 'printf "$ONLY"'];
    expect(execFileSync(shell, [flag, read], { env: { ONLY: 'given' }, encoding: 'utf8' }).trim()).toBe('given');
    let status: unknown;
    try {
      execFileSync(shell, [flag, 'exit 3'], { stdio: 'pipe' });
    } catch (error) {
      status = (error as { status: number }).status;
    }
    expect(status).toBe(3);
  });

  test('spawn streams a child and promisified exec resolves its stdout', async () => {
    const child = spawn(windows ? 'cmd.exe' : 'sh', windows ? ['/c', 'echo streamed'] : ['-c', 'echo streamed']);
    let out = '';
    child.stdout!.on('data', (chunk) => {
      out += chunk;
    });
    const code = await new Promise((resolve) => child.on('close', resolve));
    expect([code, out.trim()]).toEqual([0, 'streamed']);
    const { stdout } = await promisify(exec)('echo promised');
    expect(stdout.trim()).toBe('promised');
  });
});
