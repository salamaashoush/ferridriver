import assert from 'node:assert/strict';
import { join } from 'node:path';
import { binary, quote, repo } from './support.mjs';

export async function uiServer(cwd, args, body, timeoutMs = 30000) {
  await commands.open('ui', { command: `cd ${quote(cwd)} && exec ${[binary, ...args].map(quote).join(' ')}` });
  let drained;
  let closing = false;
  let drainError;
  try {
    let url;
    while (!url) {
      const line = await commands.read('ui', timeoutMs);
      assert.notEqual(line, null, 'UI server exited before announcing its URL');
      url = line.match(/http:\/\/127\.0\.0\.1:\d+[^\s]*/)?.[0];
    }
    drained = (async () => {
      while (await commands.read('ui', timeoutMs) !== null) {}
    })().catch(error => { if (!closing) drainError = error; });
    await body(url);
  } finally {
    closing = true;
    await commands.stop('ui');
    await drained;
  }
  if (drainError) throw drainError;
}

export async function websocket(url, body, timeoutMs = 30000) {
  await commands.open('websocket', { binary: join(repo, 'target/debug/ferridriver-websocket-probe'), url });
  try {
    assert.deepEqual(JSON.parse(await commands.read('websocket', timeoutMs)), { connected: true });
    const socket = {
      send: value => commands.write('websocket', JSON.stringify(value) + '\n'),
      async next() {
        const line = await commands.read('websocket', timeoutMs);
        assert.notEqual(line, null, 'WebSocket closed before the expected event');
        return JSON.parse(line);
      },
    };
    await body(socket);
  } finally { await commands.stop('websocket'); }
}

export async function testServer(url, body) {
  const app = new URL(url);
  const guid = app.searchParams.get('ws');
  assert.ok(guid, url);
  await websocket(`ws://${app.host}/${guid}`, async socket => {
    let nextId = 0;
    const ui = {
      events: [],
      async send(method, params = {}) {
        const id = ++nextId;
        await socket.send({ id, method, params });
        return id;
      },
      async next() {
        const message = await socket.next();
        if (message.method) ui.events.push(message);
        return message;
      },
      async waitReport(method) {
        const existing = ui.reports(method)[0];
        if (existing) return existing;
        for (;;) {
          const message = await ui.next();
          if (message.method === 'report' && message.params.method === method) return message.params;
        }
      },
      async call(method, params = {}) {
        const id = await ui.send(method, params);
        for (;;) {
          const message = await ui.next();
          if (message.id === id) {
            assert.ok(!Object.hasOwn(message, 'error'), `${method}: ${JSON.stringify(message)}`);
            return message.result ?? null;
          }
        }
      },
      reports(method) {
        return ui.events.filter(event => event.method === 'report').map(event => event.params)
          .filter(report => method === undefined || report.method === method);
      },
      socket,
    };
    await body(ui);
  });
}
