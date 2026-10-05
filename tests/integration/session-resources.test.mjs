import assert from 'node:assert/strict';
import {readFile, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {test} from '@ferridriver/test';
import {passed, quote, run, workspace} from './support.mjs';

for (const module of [false, true]) {
  for (const fails of [false, true]) {
    test(`standalone script awaits browser and command cleanup, module=${module}, fails=${fails}`, async () => {
      const root = await workspace({});
      const recorded = join(root, 'requests.json');
      const pidFile = join(root, 'worker.pid');
      const release = join(root, 'release');
      const provider = `
import json,pathlib,time
from http.server import BaseHTTPRequestHandler,HTTPServer
requests=[]
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def reply(self,value):
  self.rfile.read(int(self.headers.get('Content-Length',0)))
  requests.append([self.command,self.path])
  pathlib.Path(${JSON.stringify(recorded)}).write_text(json.dumps(requests))
  data=json.dumps({'value':value}).encode()
  self.send_response(200)
  self.send_header('Content-Type','application/json')
  self.send_header('Content-Length',len(data))
  self.end_headers()
  self.wfile.write(data)
 def do_POST(self): self.reply({'sessionId':'standalone','capabilities':{'browserName':'safari','browserVersion':'contract'}})
 def do_GET(self): self.reply([])
 def do_DELETE(self):
  print('DELETING standalone',flush=True)
  while not pathlib.Path(${JSON.stringify(release)}).exists(): time.sleep(0.005)
  self.reply(None)
server=HTTPServer(('127.0.0.1',0),Handler)
print(server.server_port,flush=True)
server.serve_forever()
`;
      await commands.start('stdio', {command: `python3 -u -c ${quote(provider)}`});
      let pending;
      try {
        const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
        const source = `
          await commands.start('worker');
          await commands.waitForOutput('worker', 'ready');
          globalThis.owned = await safari().connect('http://127.0.0.1:${port}', {timeout:1000});
          ${fails ? "throw new Error('sashoush standalone failure');" : module ? 'export default 42;' : 'return 42;'}
        `;
        const cwd = await workspace({
          'ferridriver.json': JSON.stringify({scripting: {allow: {commands: {
            worker: {run: ['sh', '-c', `echo $$ > ${quote(pidFile)}; echo ready; exec sleep 30`], persistent: true},
          }}}}),
          ...(module ? {'main.ts': source} : {}),
        });
        let acknowledged = false;
        pending = run(['run', '--no-inherit', '--json', ...(module ? ['main.ts'] : ['--eval', source])], {cwd})
          .then(result => { acknowledged = true; return result; });
        await commands.waitForOutput('stdio', 'DELETING standalone');
        const pid = Number((await readFile(pidFile, 'utf8')).trim());
        assert.ok(Number.isSafeInteger(pid) && pid > 1);
        const alive = await commands.exec('probe', {command: `python3 -c ${quote(`import os; os.kill(${pid},0)`)}`});
        assert.equal(alive.exitCode, 0, 'command stopped before its browser was released');
        assert.equal(acknowledged, false, 'script returned before browser deletion completed');
        await writeFile(release, 'release');
        const result = await pending;
        if (fails) {
          assert.notEqual(result.code, 0);
          assert.match(result.text, /sashoush standalone failure/);
        } else {
          passed(result);
          assert.equal(JSON.parse(result.stdout).value, 42);
        }
        const stopped = await commands.exec('probe', {command: `python3 -c ${quote(`
import os,sys
try: os.kill(${pid},0)
except ProcessLookupError: sys.exit(0)
sys.exit(1)
`)}`});
        assert.equal(stopped.exitCode, 0, 'script returned before reaping its owned command');
        const requests = JSON.parse(await readFile(recorded, 'utf8'));
        assert.equal(requests.filter(([method]) => method === 'POST').length, 1);
        assert.deepEqual(requests.filter(([method]) => method === 'DELETE'), [['DELETE', '/session/standalone']]);
      } finally {
        await writeFile(release, 'release');
        if (pending) await pending;
        await commands.stop('stdio');
      }
    });
  }
}

for (const retry of [false, true]) {
  test(`hosted close awaits nested browser and process cleanup, retry=${retry}`, async () => {
    const root = await workspace({});
    const recorded = join(root, 'requests.json');
    const release = join(root, 'release');
    const source = `
import json, pathlib, time
from http.server import BaseHTTPRequestHandler, HTTPServer
requests = []
created = 0
nested_deletes = 0
class Handler(BaseHTTPRequestHandler):
 def log_message(self, *args): pass
 def record(self):
  self.rfile.read(int(self.headers.get('Content-Length', 0)))
  requests.append({'method':self.command, 'path':self.path})
 def reply(self, value, status=200):
  requests[-1]['status'] = status
  pathlib.Path(${JSON.stringify(recorded)}).write_text(json.dumps(requests))
  data=json.dumps({'value':value}).encode()
  self.send_response(status)
  self.send_header('Content-Type','application/json')
  self.send_header('Content-Length',len(data))
  self.end_headers()
  self.wfile.write(data)
 def do_POST(self):
  global created
  self.record()
  if self.path == '/session':
   created += 1
   return self.reply({'sessionId':'root' if created == 1 else 'nested','capabilities':{'browserName':'safari','browserVersion':'contract'}})
  if self.path.endswith('/execute/sync'): return self.reply({'name':'','url':'about:blank','children':[]})
  self.reply(None)
 def do_GET(self):
  self.record()
  if self.path.endswith('/window/handles'): return self.reply(['page'] if '/root/' in self.path else [])
  if self.path.endswith('/window'): return self.reply('page')
  self.reply('sashoush session')
 def do_DELETE(self):
  global nested_deletes
  self.record()
  if self.path == '/session/nested':
   nested_deletes += 1
   if nested_deletes == 1:
    print('DELETING nested',flush=True)
    while not pathlib.Path(${JSON.stringify(release)}).exists(): time.sleep(0.005)
    if ${retry ? 'True' : 'False'}: return self.reply({'error':'unknown error','message':'nested cleanup unavailable'},500)
  self.reply(None)
server=HTTPServer(('127.0.0.1',0),Handler)
print(server.server_port,flush=True)
server.serve_forever()
`;
    await commands.start('stdio', {command: `python3 -u -c ${quote(source)}`});
    const id = `resources-${retry}`;
    const registry = await workspace({});
    const cwd = await workspace({'ferridriver.json': JSON.stringify({
      scripting: {allow: {commands: {worker: {run: ['sh', '-c', 'exec sleep 30'], persistent: true}}}},
    })});
    const command = args => run(args, {cwd, env: {FERRIDRIVER_SESSION_DIR: registry, FERRIDRIVER_NO_INHERIT: '1'}});
    let live = false;
    let pendingClose;
    try {
      const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
      const endpoint = `http://127.0.0.1:${port}`;
      passed(await command(['session', 'open', id, '--headless', '--backend', 'webdriver', '--connect', endpoint]));
      live = true;
      const launched = await command(['run', '--session', id, '--json', '--eval', `
        globalThis.nested = await safari().connect(${JSON.stringify(endpoint)}, {timeout:1000});
        return await commands.start('worker');
      `]);
      passed(launched);
      const {pid} = JSON.parse(launched.stdout).value;
      assert.ok(Number.isSafeInteger(pid) && pid > 1);
      let acknowledged = false;
      pendingClose = command(['session', 'close', id]).then(result => { acknowledged = true; return result; });
      await commands.waitForOutput('stdio', 'DELETING nested');
      const processState = async () => commands.exec('probe', {command: `python3 -c ${quote(`
import os,sys
try: os.kill(${pid},0)
except ProcessLookupError: sys.exit(1)
sys.exit(0)
`)}`});
      assert.equal((await processState()).exitCode, 0, 'possible provider processes must survive pending browser deletion');
      assert.equal(acknowledged, false, 'close acknowledged before provider deletion completed');
      await writeFile(release, 'release');
      const closed = await pendingClose;
      if (retry) {
        assert.notEqual(closed.code, 0);
        assert.match(closed.text, /nested cleanup unavailable/);
        assert.equal((await processState()).exitCode, 0, 'failed browser cleanup must preserve its possible provider');
        const rejected = await command(['run', '--session', id, '--eval', 'return 42;']);
        assert.notEqual(rejected.code, 0);
        assert.match(rejected.text, /closing/);
        passed(await command(['session', 'close', id]));
      } else {
        passed(closed);
      }
      live = false;
      assert.equal((await processState()).exitCode, 1, 'close returned before reaping its owned command');
      const requests = JSON.parse(await readFile(recorded, 'utf8'));
      assert.equal(requests.filter(request => request.method === 'POST' && request.path === '/session').length, 2);
      assert.equal(requests.filter(request => request.method === 'DELETE' && request.path === '/session/nested').length, retry ? 2 : 1);
      assert.equal(requests.filter(request => request.method === 'DELETE' && request.path === '/session/root').length, 1);
      const listed = await command(['session', 'list', '--json']);
      passed(listed);
      assert.deepEqual(JSON.parse(listed.stdout), []);
    } finally {
      await writeFile(release, 'release');
      if (pendingClose) await pendingClose;
      if (live) passed(await command(['session', 'close', id]));
      await commands.stop('stdio');
    }
  });
}

test('hosted cleanup retains a provider shared across script contexts until DELETE succeeds', async () => {
  const root = await workspace({});
  const recorded = join(root, 'provider.json');
  const source = `
import json,pathlib,time
from http.server import BaseHTTPRequestHandler,HTTPServer
requests=[]
deletes=0
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def record(self):
  self.rfile.read(int(self.headers.get('Content-Length',0)))
  requests.append({'method':self.command,'path':self.path})
 def reply(self,value,status=200):
  requests[-1]['status']=status
  pathlib.Path(${JSON.stringify(recorded)}).write_text(json.dumps(requests))
  data=json.dumps({'value':value}).encode()
  self.send_response(status)
  self.send_header('Content-Type','application/json')
  self.send_header('Content-Length',len(data))
  self.end_headers()
  self.wfile.write(data)
 def do_POST(self):
  self.record()
  self.reply({'sessionId':'owned','capabilities':{'browserName':'safari','browserVersion':'contract'}})
 def do_GET(self):
  self.record()
  self.reply([])
 def do_DELETE(self):
  global deletes
  self.record()
  deletes+=1
  time.sleep(0.1)
  if deletes==1: self.reply({'error':'unknown error','message':'owned provider retry'},500)
  else: self.reply(None)
server=HTTPServer(('127.0.0.1',0),Handler)
print(server.server_port,flush=True)
server.serve_forever()
`;
  const cwd = await workspace({'ferridriver.json': JSON.stringify({
    scripting: {allow: {commands: {driver: {run: ['python3', '-u', '-c', source], persistent: true}}}},
  })});
  const registry = await workspace({});
  const id = 'owned-provider';
  const command = args => run(args, {cwd, env: {FERRIDRIVER_SESSION_DIR: registry, FERRIDRIVER_NO_INHERIT: '1'}});
  let live = false;
  try {
    passed(await command(['session', 'open', id, '--headless']));
    live = true;
    const started = await command(['run', '--session', id, '--context', 'provider', '--json', '--eval', `
      const provider = await commands.start('driver');
      const port = Number((await commands.waitForOutput('driver', '\\n')).trim());
      return {pid:provider.pid, port};
    `]);
    passed(started);
    const {pid, port} = JSON.parse(started.stdout).value;
    assert.ok(Number.isSafeInteger(pid) && pid > 1);
    const connected = await command(['run', '--session', id, '--context', 'consumer', '--json', '--eval', `
      globalThis.nested = await safari().connect('http://127.0.0.1:${port}', {timeout:1000});
      return nested.version();
    `]);
    passed(connected);
    assert.equal(JSON.parse(connected.stdout).value, 'safari/contract');
    const failed = await command(['session', 'close', id]);
    assert.notEqual(failed.code, 0);
    assert.match(failed.text, /owned provider retry/);
    const alive = await commands.exec('probe', {command: `python3 -c ${quote(`import os; os.kill(${pid},0)`)}`});
    assert.equal(alive.exitCode, 0, 'cleanup killed the provider needed for retry');
    passed(await command(['session', 'close', id]));
    live = false;
    const stopped = await commands.exec('probe', {command: `python3 -c ${quote(`
import os,sys
try: os.kill(${pid},0)
except ProcessLookupError: sys.exit(0)
sys.exit(1)
`)}`});
    assert.equal(stopped.exitCode, 0, 'successful close did not reap the owned provider');
    const requests = JSON.parse(await readFile(recorded, 'utf8'));
    assert.equal(requests.filter(request => request.method === 'POST').length, 1);
    assert.deepEqual(requests.filter(request => request.method === 'DELETE').map(request => request.status), [500, 200]);
  } finally {
    if (live) passed(await command(['session', 'close', id]));
  }
});
