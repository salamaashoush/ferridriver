import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { quote, repo, run, workspace } from './support.mjs';

for (const [backend, version] of [['cdp-pipe', 'Chrome/'], ['cdp-raw', 'Chrome/'], ['bidi', 'firefox/'], ['webkit', 'webkit-playwright/']]) {
  test(`a configured ${backend} test target selects the matching browser`, async () => {
    const cwd = await workspace({
      'target.test.ts': `import {test, expect} from '@ferridriver/test';
        test('target', async ({browser, page}) => {
          expect(browser.version()).toContain(${JSON.stringify(version)});
          await page.setContent('<title>sashoush</title>');
          expect(await page.title()).toBe('sashoush');
        });`,
      'ferridriver.json': JSON.stringify({
        browser: { instances: { target: { backend, headless: true } } },
        test: { workers: 1, retries: 0, browser: { instance: 'target' } },
      }),
    });
    const result = await run(['test', '--no-inherit', '--config', join(cwd, 'ferridriver.json'), 'target.test.ts'], { cwd });
    assert.equal(result.code, 0, result.text);
    assert.match(result.text, /1 passed/);
  });
}

test('a test project creates its configured remote session instead of launching locally', async () => {
  const cwd = await workspace({
    'target.test.ts': "import {test} from '@ferridriver/test'; test('target', async ({page}) => { await page.title(); });",
  });
  const recorded = join(cwd, 'request.json');
  const source = `
from http.server import BaseHTTPRequestHandler, HTTPServer
class Handler(BaseHTTPRequestHandler):
 def do_POST(self):
  body = self.rfile.read(int(self.headers['Content-Length']))
  open(${JSON.stringify(recorded)}, 'wb').write(body)
  response = b'{"value":{"error":"session not created","message":"target-contract-refused contract-private-token"}}'
  self.send_response(500)
  self.send_header('Content-Type', 'application/json')
  self.send_header('Content-Length', str(len(response)))
  self.end_headers()
  self.wfile.write(response)
 def log_message(self, *args): pass
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.handle_request()
server.server_close()
`;
  await commands.start('stdio', { command: `python3 -u -c ${quote(source)}` });
  try {
    const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
    assert.ok(port > 0);
    await writeFile(join(cwd, 'ferridriver.json'), JSON.stringify({
      browser: { instances: { remote: {
        connectUrl: `http://127.0.0.1:${port}/wd/hub`,
        connectOptions: { timeout: 3000, capabilities: { browserName: 'chrome', 'example:target': 'sashoush', 'example:token': 'contract-private-token' } },
      } } },
      test: { workers: 1, retries: 0, browser: { instance: 'remote' } },
    }));
    const result = await run(['test', '--no-inherit', '--config', join(cwd, 'ferridriver.json'), 'target.test.ts'], { cwd });
    assert.notEqual(result.code, 0, result.text);
    assert.match(result.text, /target-contract-refused/);
    assert.doesNotMatch(result.text, /contract-private-token/);
    const body = JSON.parse(await readFile(recorded, 'utf8'));
    assert.equal(body.capabilities.alwaysMatch.browserName, 'chrome');
    assert.equal(body.capabilities.alwaysMatch['example:target'], 'sashoush');
  } finally { await commands.stop('stdio'); }
});

test('a passing test fails the run when its owned remote session cannot be deleted', async () => {
  const cwd = await workspace({
    'target.test.ts': "import {test, expect} from '@ferridriver/test'; test('target', async ({browser}) => { expect(browser.version()).toBe('firefox/contract'); });",
  });
  await commands.start('stdio', {
    command: `bun ${quote(join(repo, 'tests/integration/fixtures/webdriver-cleanup-server.mjs'))}`,
  });
  try {
    const port = Number((await commands.waitForOutput('stdio', '\n')).trim());
    assert.ok(port > 0);
    await writeFile(join(cwd, 'ferridriver.json'), JSON.stringify({
      browser: { instances: { remote: {
        connectUrl: `http://127.0.0.1:${port}/wd/hub`,
        connectOptions: { timeout: 3000, capabilities: { browserName: 'firefox' } },
      } } },
      test: { workers: 1, retries: 0, browser: { instance: 'remote' } },
    }));
    const result = await run(['test', '--no-inherit', '--config', join(cwd, 'ferridriver.json'), 'target.test.ts'], { cwd });
    assert.equal(result.code, 1, result.text);
    assert.match(result.text, /1 passed/);
    assert.match(result.text, /worker cleanup failed.*WebDriver DELETE \/session/);
  } finally { await commands.stop('stdio'); }
});
