import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, quote, repo, run, workspace } from './support.mjs';

for (const protocol of ['cdp', 'bidi']) {
  test(`${protocol} connection close retires retained handles without closing the provider`, async () => {
    const fixture = join(repo, 'tests/shared/protocol-close-server.mjs');
    await commands.start('stdio', { command: `exec bun ${quote(fixture)}` });
    try {
      const output = await commands.waitForOutput('stdio', '\n');
      const { port } = JSON.parse(output.trim());
      assert.ok(Number.isInteger(port) && port > 0, output);
      const endpoint = `ws://127.0.0.1:${port}/${protocol}`;
      const status = `http://127.0.0.1:${port}/status?protocol=${protocol}`;
      const connect = protocol === 'cdp'
        ? `chromium().connectOverCDP(${JSON.stringify(endpoint)})`
        : `firefox().connect(${JSON.stringify(endpoint)})`;
      const source = `
        const browser = await ${connect};
        const page = (await browser.contexts()[0].pages())[0];
        const session = ${protocol === 'cdp' ? 'await browser.newBrowserCDPSession()' : 'null'};
        const before = { connected: browser.isConnected(), title: await page.title() };
        await browser.close();
        async function observe(operation) {
          try { return { resolved: true, value: await operation() }; }
          catch (error) { return { resolved: false, error: String(error) }; }
        }
        const stalePage = await observe(() => page.title());
        const staleSession = session ? await observe(() => session.send('Browser.getVersion')) : null;
        const response = await fetch(${JSON.stringify(status)});
        return {
          before,
          connected: browser.isConnected(),
          pageClosed: page.isClosed(),
          stalePage,
          staleSession,
          providerStatus: response.status,
          socket: await response.json(),
        };
      `;
      const result = await run(['run', '--no-inherit', '--json', '-e', source], { cwd: await workspace({}) });
      passed(result);
      const response = JSON.parse(result.stdout);
      assert.equal(response.status, 'ok', result.text);
      const value = response.value;
      assert.deepEqual(value.before, { connected: true, title: 'sashoush fixture' });
      assert.equal(value.connected, false);
      assert.equal(value.pageClosed, true);
      assert.equal(value.stalePage.resolved, false, JSON.stringify(value.stalePage));
      assert.match(value.stalePage.error, /closed|disconnected/i);
      if (protocol === 'cdp') {
        assert.equal(value.staleSession.resolved, false, JSON.stringify(value.staleSession));
        assert.match(value.staleSession.error, /closed|disconnected/i);
      }
      assert.equal(value.providerStatus, 200);
      assert.equal(value.socket.opened, 1);
      assert.equal(value.socket.closed, 1);
      assert.equal(value.socket.active, 0);
      assert.equal(value.socket.commands.includes('Browser.close'), false);
      assert.equal(value.socket.commands.includes('browser.close'), false);
      assert.equal((await commands.status('stdio')).running, true);
    } finally {
      await commands.stop('stdio');
    }
  });
}
