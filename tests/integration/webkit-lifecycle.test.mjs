import assert from 'node:assert/strict';
import {chmod, mkdir, readFile, readdir} from 'node:fs/promises';
import {join} from 'node:path';
import {test} from '@ferridriver/test';
import {quote, run, workspace} from './support.mjs';

test('standalone script awaits WebKit process and directory cleanup after startup rejection', async () => {
  const root = await workspace({});
  const pidFile = join(root, 'pid');
  const closedFile = join(root, 'closed');
  const cwd = await workspace({'webkit.py': `#!/usr/bin/env python3
import json,os,pathlib
pathlib.Path(${JSON.stringify(pidFile)}).write_text(str(os.getpid()))
reader=os.fdopen(3,'rb',buffering=0)
writer=os.fdopen(4,'wb',buffering=0)
frame=bytearray()
while True:
 byte=reader.read(1)
 if not byte: break
 if byte != b'\\x00':
  frame.extend(byte)
  continue
 request=json.loads(frame)
 frame.clear()
 if request['method']=='Playwright.close':
  pathlib.Path(${JSON.stringify(closedFile)}).write_text('closed')
  break
 writer.write(json.dumps({'id':request['id'],'error':{'message':'sashoush startup rejected'}}).encode()+b'\\x00')
`});
  const executable = join(cwd, 'webkit.py');
  const temporary = join(root, 'tmp');
  await chmod(executable, 0o700);
  await mkdir(temporary);
  const result = await run(['run', '--no-inherit', '--json', '--eval', 'await webkit().launch({headless:true}); return 42;'], {
    cwd, env: {FERRIDRIVER_WEBKIT: executable, TMPDIR: temporary},
  });
  assert.notEqual(result.code, 0);
  assert.match(result.text, /sashoush startup rejected/);
  assert.equal(await readFile(closedFile, 'utf8'), 'closed');
  assert.deepEqual((await readdir(temporary)).filter(name => name.startsWith('ferridriver-webkit-downloads-')), []);
  const pid = Number((await readFile(pidFile, 'utf8')).trim());
  assert.ok(Number.isSafeInteger(pid) && pid > 1);
  const reaped = await commands.exec('probe', {command: `python3 -c ${quote(`
import os,sys
try: os.kill(${pid},0)
except ProcessLookupError: sys.exit(0)
sys.exit(1)
`)}`});
  assert.equal(reaped.exitCode, 0, 'startup error returned without reaping WebKit');
});
