import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, passed, quote, run, runtimeProbe, workspace } from './support.mjs';

for (const [status, classic] of [[200, false], [404, false], [401, false], [403, false], [429, false], [503, false], [200, true]]) {
  test(`Android scripting preserves its session during protocol selection, extension status=${status}, Classic=${classic}`, async () => {
    const root = await workspace({});
    const recorded = join(root, 'android-requests.json');
    const chrome = status === 200 ? {browserName: 'chrome', browserVersion: '152.0.7977.82'} :
      {error: 'unsupported operation', message: 'provider rejected extension'};
    const version = status === 200 ? '' : '152.0.7977.82';
    const server = `
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
requests = []
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, value, status=200):
        with open(${JSON.stringify(recorded)}, 'w') as output: json.dump(requests, output)
        response = json.dumps({'value': value}).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.end_headers()
        self.wfile.write(response)
    def record(self):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        body = json.loads(body) if body else None
        requests.append({'method': self.command, 'path': self.path, 'authorization': self.headers.get('Authorization'), 'body': body})
        return body
    def do_POST(self):
        body = self.record()
        if self.path == '/wd/hub/session':
            return self.reply({'sessionId': 'android', 'capabilities': {'browserName': 'Chrome', 'browserVersion': ${JSON.stringify(version)}}})
        if body.get('script') == 'mobile: getChromeCapabilities':
            return self.reply(json.loads(${JSON.stringify(JSON.stringify(chrome))}), ${status})
        self.reply(None)
    def do_GET(self):
        self.record()
        self.reply(['page'] if self.path.endswith('/window/handles') else "sashoush's Android session")
    def do_DELETE(self):
        self.record()
        self.reply(None)
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
`;
    await commands.start('stdio', {command: `python3 -u -c ${quote(server)}`});
    try {
      const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
      assert.ok(port > 0);
      const {results} = await runtimeProbe([{op: 'execute-script', source: `
        try {
          const browser = await chromium().connect('http://127.0.0.1:${port}/wd/hub', {
            timeout: 1000, headers: {authorization: 'Bearer contract-token'},
            capabilities: {platformName: 'Android', 'appium:options': {automationName: 'UiAutomator2'}, ${classic ? 'webSocketUrl: false' : ''}},
          });
          try {
            const pages = await browser.contexts()[0].pages();
            return {version: browser.version(), title: await pages[0].title()};
          } finally {await browser.close();}
        } catch (error) {return {error: String(error)};}
      `}]);
      const result = observation(results[0]);
      assert.equal(result.status, 'ok', JSON.stringify(result));
      if (status === 200 || status === 404) {
        assert.deepEqual(result.value, {version: 'Chrome/152.0.7977.82', title: "sashoush's Android session"});
      } else {
        assert.match(result.value.error, new RegExp(String(status)));
      }
      const requests = JSON.parse(await readFile(recorded, 'utf8'));
      assert.equal(requests.filter(request => request.path === '/wd/hub/session').length, 1);
      assert.equal(requests.filter(request => request.body?.script === 'mobile: getChromeCapabilities').length, 1);
      assert.deepEqual(requests.filter(request => request.method === 'DELETE').map(request => request.path), ['/wd/hub/session/android']);
      assert.ok(requests.every(request => request.authorization === 'Bearer contract-token'));
      if (status !== 200 && status !== 404) assert.deepEqual(requests.map(request => request.method), ['POST', 'POST', 'DELETE']);
    } finally {await commands.stop('stdio');}
  });
}

for (const [factory, browserName] of [['chromium', 'chrome'], ['firefox', 'firefox']]) {
  for (const socket of [undefined, false]) {
    test(`${factory} scripting adopts a Classic session, webSocketUrl=${socket}`, async () => {
      const root = await workspace({});
      const recorded = join(root, 'requests.json');
      const server = `
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
requests = []
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, value):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        requests.append({'method': self.command, 'path': self.path, 'authorization': self.headers.get('Authorization'), 'body': json.loads(body) if body else None})
        with open(${JSON.stringify(recorded)}, 'w') as output: json.dump(requests, output)
        response = json.dumps({'value': value}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.end_headers()
        self.wfile.write(response)
    def do_POST(self):
        self.reply({'sessionId': 'owned', 'capabilities': {'browserName': ${JSON.stringify(browserName)}, 'browserVersion': 'contract'}} if self.path == '/wd/hub/session' else None)
    def do_GET(self):
        self.reply(['page'] if self.path.endswith('/window/handles') else "sashoush's Classic session")
    def do_DELETE(self): self.reply(None)
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
`;
      await commands.start('stdio', {command: `python3 -u -c ${quote(server)}`});
      try {
        const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
        assert.ok(port > 0);
        const capabilities = {'acme:options': {region: 'test'}, ...(socket === undefined ? {} : {webSocketUrl: socket})};
        const {results} = await runtimeProbe([{op: 'execute-script', source: `
          const browser = await ${factory}().connect('http://127.0.0.1:${port}/wd/hub', {
            timeout: 1000, headers: {authorization: 'Bearer contract-token'}, capabilities: ${JSON.stringify(capabilities)},
          });
          try {
            const pages = await browser.contexts()[0].pages();
            return {version: browser.version(), count: pages.length, title: await pages[0].title()};
          } finally {await browser.close();}
        `}]);
        const result = observation(results[0]);
        assert.equal(result.status, 'ok', JSON.stringify(result));
        assert.deepEqual(result.value, {version: `${browserName}/contract`, count: 1, title: "sashoush's Classic session"});
        const requests = JSON.parse(await readFile(recorded, 'utf8'));
        const creations = requests.filter(request => request.method === 'POST' && request.path === '/wd/hub/session');
        assert.equal(creations.length, 1);
        const negotiated = creations[0].body.capabilities.alwaysMatch;
        assert.equal(negotiated.webSocketUrl, socket ?? true);
        assert.deepEqual(negotiated['acme:options'], {region: 'test'});
        assert.ok(requests.every(request => request.authorization === 'Bearer contract-token'));
        assert.deepEqual(requests.filter(request => request.method === 'DELETE').map(request => request.path), ['/wd/hub/session/owned']);
      } finally {await commands.stop('stdio');}
    });
  }
}

test('XCUITest script keyboard input uses native context and restores the web target', async () => {
  const root = await workspace({});
  const recorded = join(root, 'native-input.json');
  const server = `
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
state = {'context': 'WEBVIEW_device', 'inputContext': None, 'keys': []}
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, value, status=200):
        with open(${JSON.stringify(recorded)}, 'w') as output: json.dump(state, output)
        response = json.dumps({'value': value}).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.end_headers()
        self.wfile.write(response)
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))))
        if self.path == '/session':
            return self.reply({'sessionId': 'input', 'capabilities': {'browserName': 'Safari', 'browserVersion': '26.2', 'platformName': 'iOS', 'automationName': 'XCUITest'}})
        if self.path.endswith('/context'): state['context'] = body['name']
        if self.path.endswith('/execute/sync'):
            return self.reply(True if 'document.visibilityState' in body['script'] else None)
        if self.path.endswith('/actions'):
            state['inputContext'] = state['context']
            if state['context'] != 'NATIVE_APP':
                return self.reply({'error': 'unknown error', 'message': 'input reached web backend'}, 500)
            state['keys'].extend(action['type'] + ':' + action['value'] for source in body['actions'] for action in source['actions'] if action['type'] in ['keyDown', 'keyUp'])
        self.reply(None)
    def do_GET(self): self.reply(['device'] if self.path.endswith('/window/handles') else None)
    def do_DELETE(self): self.reply(None)
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
`;
  await commands.start('stdio', {command: `python3 -u -c ${quote(server)}`});
  try {
    const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
    assert.ok(port > 0);
    const {results} = await runtimeProbe([{op: 'execute-script', source: `
      const browser = await safari().connect('http://127.0.0.1:${port}', {timeout: 1000});
      try {
        const page = (await browser.contexts()[0].pages())[0];
        await page.keyboard.press('a');
        return true;
      } finally { await browser.close(); }
    `}]);
    const response = observation(results[0]);
    assert.equal(response.status, 'ok', JSON.stringify(response));
    assert.equal(response.value, true);
    assert.deepEqual(JSON.parse(await readFile(recorded, 'utf8')), {
      context: 'WEBVIEW_device', inputContext: 'NATIVE_APP', keys: ['keyDown:a', 'keyUp:a'],
    });
  } finally { await commands.stop('stdio'); }
});

test('worker browser launch has its own deadline and preserves the test body timeout', async () => {
  const root = await workspace({});
  const deleted = join(root, 'deleted.json');
  const server = `
import json, time
from http.server import BaseHTTPRequestHandler, HTTPServer
deletions = 0
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, value):
        self.rfile.read(int(self.headers.get('Content-Length', 0)))
        response = json.dumps({'value': value}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.end_headers()
        self.wfile.write(response)
    def do_POST(self):
        time.sleep(1.5)
        self.reply({'sessionId': 'worker', 'capabilities': {'browserName': 'safari', 'browserVersion': '26.2', 'setWindowRect': False}})
    def do_GET(self): self.reply([])
    def do_DELETE(self):
        global deletions
        deletions += 1
        open(${JSON.stringify(deleted)}, 'w').write(str(deletions))
        self.reply(None)
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
`;
  await commands.start('stdio', {command: `python3 -u -c ${quote(server)}`});
  try {
    const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
    assert.ok(port > 0);
    const config = JSON.stringify({
      browser: {instances: {target: {browser: 'safari', connectUrl: `http://127.0.0.1:${port}`,
        connectOptions: {timeout: 5000}}}},
      test: {timeout: 500, workers: 1, browser: {instance: 'target'}},
    });
    for (const hangs of [false, true]) {
      const cwd = await workspace({
        'ferridriver.json': config,
        'launch.test.ts': `import {test, expect} from '@ferridriver/test';
          test('browser setup', async ({browser}) => {
            expect(browser.version()).toBe('safari/26.2');
            ${hangs ? 'await new Promise(() => {});' : ''}
          });`,
      });
      const result = await run(['test', '--no-inherit', 'launch.test.ts'], {cwd});
      if (hangs) {
        assert.equal(result.code, 1, result.text);
        assert.match(result.text, /timed out/);
      } else {
        passed(result);
      }
      assert.equal(await readFile(deleted, 'utf8'), hangs ? '2' : '1');
    }
  } finally {await commands.stop('stdio');}
});

test('Classic scripting discovers reordered windows and invalidates closed pages', async () => {
  const server = `
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
handles = iter([['old'], ['old'], ['new', 'old'], ['new'], ['replacement']])
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, value):
        self.rfile.read(int(self.headers.get('Content-Length', 0)))
        response = json.dumps({'value': value}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.end_headers()
        self.wfile.write(response)
    def do_POST(self):
        self.reply({'sessionId': 'pages', 'capabilities': {'browserName': 'Safari', 'browserVersion': '26.2', 'setWindowRect': False}})
    def do_GET(self): self.reply(next(handles))
    def do_DELETE(self): self.reply(None)
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
for _ in range(7): server.handle_request()
server.server_close()
`;
  await commands.start('stdio', { command: `python3 -u -c ${quote(server)}` });
  try {
    const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
    assert.ok(port > 0);
    const { results } = await runtimeProbe([{
      op: 'execute-script',
      source: `
        const browser = await safari().connect('http://127.0.0.1:${port}', {timeout: 1000});
        try {
          const context = browser.contexts()[0];
          const old = (await context.pages())[0];
          const discovered = await context.pages();
          const count = discovered.length;
          const afterClose = (await context.pages()).length;
          const oldClosed = old.isClosed(), newOpen = !discovered[1].isClosed();
          const replacement = (await context.pages()).length;
          return {count, afterClose, oldClosed, newOpen, replacement, replacedClosed: discovered[1].isClosed()};
        } finally {await browser.close();}
      `,
    }]);
    const response = observation(results[0]);
    assert.equal(response.status, 'ok', JSON.stringify(response));
    assert.deepEqual(response.value, {
      count: 2, afterClose: 1, oldClosed: true, newOpen: true, replacement: 1, replacedClosed: true,
    });
  } finally {await commands.stop('stdio');}
});

test('native scripting requests BiDi and forwards vendor capabilities and headers', async () => {
  const root = await workspace({});
  const recorded = join(root, 'webdriver-request.bin');
  const deleted = join(root, 'webdriver-delete.bin');
  const server = `
import socket
server = socket.socket()
server.bind(('127.0.0.1', 0))
server.listen(1)
print(server.getsockname()[1], flush=True)
client, _ = server.accept()
data = b''
while b'\\r\\n\\r\\n' not in data:
    chunk = client.recv(4096)
    if not chunk: break
    data += chunk
head, _, body = data.partition(b'\\r\\n\\r\\n')
length = 0
for line in head.split(b'\\r\\n'):
    if line.lower().startswith(b'content-length:'):
        length = int(line.split(b':', 1)[1].strip())
while len(body) < length:
    body += client.recv(4096)
open(${JSON.stringify(recorded)}, 'wb').write(head + b'\\r\\n\\r\\n' + body)
response = b'{\"value\":{\"sessionId\":\"mobile-contract\",\"capabilities\":{\"browserName\":\"safari\"}}}'
client.sendall(b'HTTP/1.1 200 OK\\r\\nContent-Type: application/json\\r\\nContent-Length: ' + str(len(response)).encode() + b'\\r\\nConnection: close\\r\\n\\r\\n' + response)
client.close()
client, _ = server.accept()
data = b''
while b'\\r\\n\\r\\n' not in data:
    chunk = client.recv(4096)
    if not chunk: break
    data += chunk
open(${JSON.stringify(deleted)}, 'wb').write(data)
response = b'{"value":null}'
client.sendall(b'HTTP/1.1 200 OK\\r\\nContent-Type: application/json\\r\\nContent-Length: ' + str(len(response)).encode() + b'\\r\\nConnection: close\\r\\n\\r\\n' + response)
client.close()
server.close()
`;
  await commands.start('stdio', { command: `python3 -u -c ${quote(server)}` });
  try {
    const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
    assert.ok(port > 0);
    const { results } = await runtimeProbe([{
      op: 'execute-script',
      source: `
        try {
          await firefox().connect('http://127.0.0.1:${port}', {
            headers: { authorization: 'Bearer contract-token' },
            timeout: 1000,
            capabilities: {
              platformName: 'iOS',
              webSocketUrl: true,
              'appium:options': { automationName: 'Safari', deviceName: 'example-device' },
            },
          });
          return { error: '' };
        } catch (error) {
          return { error: String(error) };
        }
      `,
    }]);
    const response = observation(results[0]);
    assert.equal(response.status, 'ok', JSON.stringify(response));
    const outcome = response.value;
    assert.ok(outcome.error, JSON.stringify(outcome));
    assert.match(outcome.error, /webSocketUrl/);
    const request = await readFile(recorded, 'utf8');
    const body = JSON.parse(request.slice(request.indexOf('\r\n\r\n') + 4));
    const capabilities = body.capabilities.alwaysMatch;
    assert.equal(capabilities.platformName, 'iOS');
    assert.equal(capabilities.webSocketUrl, true);
    assert.deepEqual(capabilities['appium:options'], { automationName: 'Safari', deviceName: 'example-device' });
    assert.match(request, /authorization: Bearer contract-token/i);
    const cleanup = await readFile(deleted, 'utf8');
    assert.match(cleanup, /^DELETE \/session\/mobile-contract HTTP\/1.1/);
    assert.match(cleanup, /authorization: Bearer contract-token/i);
  } finally {
    await commands.stop('stdio');
  }
});

test('native Safari scripting uses Classic WebDriver and closes its session', async () => {
  const root = await workspace({});
  const recorded = join(root, 'classic-requests.json');
  const server = `
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
requests = []
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def reply(self, value):
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        requests.append({'method': self.command, 'path': self.path, 'body': json.loads(body) if body else None})
        with open(${JSON.stringify(recorded)}, 'w') as output: json.dump(requests, output)
        response = json.dumps({'value': value}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', len(response))
        self.end_headers()
        self.wfile.write(response)
    def do_POST(self):
        self.reply({'sessionId': 'classic-contract', 'capabilities': {'browserName': 'Safari', 'browserVersion': '26.2', 'platformName': 'iOS', 'setWindowRect': False}})
    def do_GET(self): self.reply(['device'])
    def do_DELETE(self): self.reply(None)
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
for _ in range(3): server.handle_request()
server.server_close()
`;
  await commands.start('stdio', { command: `python3 -u -c ${quote(server)}` });
  try {
    const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
    assert.ok(port > 0);
    const { results } = await runtimeProbe([{
      op: 'execute-script',
      source: `
        const browser = await safari().connect('http://127.0.0.1:${port}', {
          timeout: 1000, capabilities: { platformName: 'iOS', 'safari:useSimulator': true },
        });
        try { return { version: browser.version() }; }
        finally { await browser.close(); }
      `,
    }]);
    const response = observation(results[0]);
    assert.equal(response.status, 'ok', JSON.stringify(response));
    assert.deepEqual(response.value, { version: 'Safari/26.2' });
    const requests = JSON.parse(await readFile(recorded, 'utf8'));
    assert.deepEqual(requests.map(request => [request.method, request.path]), [
      ['POST', '/session'], ['GET', '/session/classic-contract/window/handles'], ['DELETE', '/session/classic-contract'],
    ]);
    assert.deepEqual(requests[0].body.capabilities.alwaysMatch, {
      browserName: 'safari', unhandledPromptBehavior: 'ignore', platformName: 'iOS', 'safari:useSimulator': true,
    });
  } finally {
    await commands.stop('stdio');
  }
});
