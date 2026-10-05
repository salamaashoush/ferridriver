import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {test} from '@ferridriver/test';
import {passed, quote, run, workspace} from './support.mjs';

for (const timeout of [0, 60000]) {
  test(`native WebMCP scripting forwards its ${timeout}ms budget to Classic script timeouts`, async () => {
    const root = await workspace({});
    const recorded = join(root, 'requests.json');
    const source = `
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
requests = []
script_timeout = 30000
class Handler(BaseHTTPRequestHandler):
 def log_message(self, *args): pass
 def reply(self, value):
  with open(${JSON.stringify(recorded)}, 'w') as output: json.dump(requests, output)
  data=json.dumps({'value':value}).encode()
  self.send_response(200)
  self.send_header('Content-Type','application/json')
  self.send_header('Content-Length',len(data))
  self.end_headers()
  self.wfile.write(data)
 def do_POST(self):
  global script_timeout
  data=self.rfile.read(int(self.headers.get('Content-Length',0)))
  body=json.loads(data) if data else {}
  requests.append({'method':'POST','path':self.path,'timeout':body.get('script') if self.path.endswith('/timeouts') else None})
  if self.path.endswith('/timeouts'): script_timeout=body['script']
  if self.path == '/session': return self.reply({'sessionId':'budget','capabilities':{'browserName':'firefox','browserVersion':'155.0.1'}})
  if self.path.endswith('/execute/sync'):
   args=body.get('args',[])
   if len(args)==6 and 'const toolName' in str(args[2]):
    requests.append({'method':'WEBMCP','path':self.path,'timeout':script_timeout})
    return self.reply({'kind':'value','value':json.dumps({'a':[True,{'a':[],'id':2}],'id':1})})
   return self.reply({'name':'','url':'about:blank','children':[]})
  self.reply(None)
 def do_GET(self):
  requests.append({'method':'GET','path':self.path})
  self.reply(['page'] if self.path.endswith('/window/handles') else 'page')
 def do_DELETE(self):
  requests.append({'method':'DELETE','path':self.path})
  self.reply(None)
server=HTTPServer(('127.0.0.1',0),Handler)
print(server.server_port,flush=True)
server.serve_forever()
`;
    await commands.start('stdio', {command: `python3 -u -c ${quote(source)}`});
    try {
      const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
      const result = await run(['run', '--no-inherit', '--json', '--eval', `
        const remote = await firefox().connect('http://127.0.0.1:${port}', {timeout:1000, capabilities:{webSocketUrl:false}});
        try {
          const pages = await remote.contexts()[0].pages();
          return await pages[0].webmcp.tools({timeout:${timeout}});
        } finally {await remote.close();}
      `]);
      passed(result);
      assert.deepEqual(JSON.parse(result.stdout).value, []);
      const requests = JSON.parse(await readFile(recorded, 'utf8'));
      const settings = requests.filter(request => request.path.endsWith('/timeouts'));
      assert.ok(settings.length > 0, JSON.stringify(requests));
      const invocation = requests.filter(request => request.method === 'WEBMCP');
      assert.equal(invocation.length, 1, JSON.stringify(requests));
      if (timeout === 0) assert.equal(invocation[0].timeout, null);
      else assert.ok(invocation[0].timeout > 30000 && invocation[0].timeout <= timeout, JSON.stringify(invocation));
      assert.equal(requests.filter(request => request.path === '/session').length, 1);
      assert.deepEqual(requests.filter(request => request.method === 'DELETE').map(request => request.path), ['/session/budget']);
    } finally {await commands.stop('stdio');}
  });
}
