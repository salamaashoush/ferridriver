import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { listZipEntries, readZipEntry } from '../e2e/helpers/unzip.ts';
import { workspace } from './support.mjs';
import { fixtureServer } from './fixture-server.mjs';
import { McpClient } from './mcp-client.mjs';

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP records a complete HAR archive and replays it with the origin offline`, async () => {
    const server = await fixtureServer();
    const client = await McpClient.launch(backend);
    let serverClosed = false;
    try {
      const path = join(await workspace({}), 'recording.har.zip');
      const url = `${server.url}/fx/har`;
      const recorded = await client.script(`
        await context.addCookies([{ name: 'reqcookie', value: 'reqvalue', domain: '127.0.0.1', path: '/' }]);
        await context.tracing.startHar(args[1]);
        await page.goto(args[0]);
        await context.tracing.stopHar();
        return { done: true };
      `, [url, path]);
      assert.equal(recorded.done, true);
      const bytes = await readFile(path);
      const archive = listZipEntries(bytes);
      const entry = archive.find(entry => entry.name === 'har.har');
      assert.ok(entry);
      const har = JSON.parse(new TextDecoder().decode(readZipEntry(bytes, entry)));
      const entries = har.log.entries;
      assert.ok(entries.length > 0);
      const refs = entries.map(entry => entry.response.content._file).filter(name => typeof name === 'string');
      assert.ok(refs.length > 0);
      for (const ref of refs) {
        const body = archive.find(entry => entry.name === ref);
        assert.ok(body, ref);
        readZipEntry(bytes, body);
      }
      assert.ok(entries.some(entry => entry.response.content.mimeType === 'text/html'));
      const document = entries.find(entry => entry.request.url === url);
      assert.ok(document);
      assert.ok(document.response.cookies.some(cookie => cookie.name === 'harcookie' && cookie.value === 'harvalue'));
      assert.equal(typeof document.response.httpVersion, 'string');
      assert.ok(document.response.httpVersion.length > 0);
      if (backend.startsWith('cdp')) {
        assert.equal(document.serverIPAddress, '127.0.0.1');
        assert.equal(document._serverPort, Number(new URL(server.url).port));
        for (const phase of ['dns', 'connect', 'ssl', 'send', 'wait', 'receive']) {
          assert.equal(typeof document.timings[phase], 'number', phase);
        }
      }
      assert.ok(har.log.pages.some(page => page.title === 'HAR Fixture Title'));
      if (backend !== 'webkit') assert.ok(document.request.cookies.some(cookie => cookie.name === 'reqcookie'));
      await server.close();
      serverClosed = true;
      const replay = await client.script(`
        await context.routeFromHAR(args[1], { notFound: 'abort' });
        await page.goto(args[0]);
        const served = await page.evaluate(() => document.body.textContent);
        let missThrew = false;
        try { await page.goto('http://ferri-har-miss.test/none', { timeout: 3000 }); }
        catch { missThrew = true; }
        await context.unrouteAll();
        return { served: String(served), missThrew };
      `, [url, path]);
      assert.ok(replay.served.trim().length > 0);
      assert.equal(replay.missThrew, true);
    } finally {
      await client.close();
      if (!serverClosed) await server.close();
    }
  });
}
