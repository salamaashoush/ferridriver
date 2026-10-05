import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, ok, payload } from './mcp-client.mjs';
import { quote } from './support.mjs';

test('concurrent MCP callers each receive their own correlated response', async () => {
  const client = await McpClient.launch();
  try {
    const replies = await Promise.all(Array.from({ length: 32 }, (_, index) =>
      client.call('run_script', { source: 'return args[0]', args: [index] })));
    assert.deepEqual(replies.map(reply => payload(ok(reply)).value), Array.from({ length: 32 }, (_, index) => index));
    assert.equal(new Set(replies.map(reply => reply.id)).size, 32);
  } finally {
    await client.close();
  }
});

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP close cancels an active browser wait and admits a fresh session afterward`, async () => {
    const source = `
from http.server import BaseHTTPRequestHandler,HTTPServer
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_GET(self):
  if self.path=='/started': print('STARTED',flush=True)
  data=b'<!doctype html><title>sashoush cleanup</title>'
  self.send_response(200)
  self.send_header('Content-Type','text/html')
  self.send_header('Content-Length',len(data))
  self.end_headers()
  self.wfile.write(data)
server=HTTPServer(('127.0.0.1',0),Handler)
print(server.server_port,flush=True)
server.serve_forever()
`;
    await commands.start('stdio', {command: `python3 -u -c ${quote(source)}`});
    let client;
    try {
      const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
      client = await McpClient.launch(backend);
      const pending = client.call('run_script', {
        source: `await page.goto(args[0]); await page.evaluate(async base => {
          await fetch(base + '/started');
          return await new Promise(() => {});
        }, args[0]);`,
        args: [`http://127.0.0.1:${port}`],
        timeout_ms: 60000,
      });
      await commands.waitForOutput('stdio', 'STARTED');
      ok(await client.call('page', {action: 'close_browser'}));
      const interrupted = payload(await pending);
      assert.equal(interrupted.status, 'error');
      assert.match(interrupted.error.message, /closing|closed/);
      assert.equal(await client.script('return 42;'), 42);
    } finally {
      try { if (client) await client.close(); }
      finally { await commands.stop('stdio'); }
    }
  });
}
