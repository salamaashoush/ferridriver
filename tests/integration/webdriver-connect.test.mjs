import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, quote, runtimeProbe, workspace } from './support.mjs';

test('native scripting sends mobile WebDriver capabilities and preserves headers', async () => {
  const root = await workspace({});
  const recorded = join(root, 'webdriver-request.bin');
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
          await webkit().connect('http://127.0.0.1:${port}', {
            headers: { authorization: 'Bearer contract-token' },
            timeout: 1000,
            capabilities: {
              platformName: 'iOS',
              webSocketUrl: false,
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
  } finally {
    await commands.stop('stdio');
  }
});
