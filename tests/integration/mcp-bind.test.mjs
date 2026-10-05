import assert from 'node:assert/strict';
import { readFile, writeFile, access } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run } from './support.mjs';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: a bound MCP browser serves its live page and persistent script state`, async () => {
    const client = await McpClient.launch(backend);
    const env = { FERRIDRIVER_SESSION_DIR: client.registry };
    const name = `mcp-bind-${crypto.randomUUID()}`;
    try {
      ok(await client.call('navigate', { url: dataUrl('<h1 id="greet">session-bound</h1>') }));
      const endpoint = await client.script("return (await browser.bind(args[0], { host: '127.0.0.1', port: 0 })).endpoint;", [name]);
      assert.match(endpoint, /^ws:\/\/127\.0\.0\.1:/);
      const execute = async source => {
        const result = await run(['run', '--session', name, '--eval', source, '--json', '--no-inherit'], { env });
        passed(result);
        const value = JSON.parse(result.stdout);
        assert.equal(value.status, 'ok', result.text);
        return value.value;
      };
      assert.match(JSON.stringify(await execute('return await page.snapshotForAI();')), /session-bound/);
      assert.match(await execute('return page.url();'), /^data:/);
      await execute("globalThis.bound = 'kept'; return null;");
      assert.equal(await execute('return globalThis.bound;'), 'kept');
      const descriptorPath = join(client.registry, `${name}.json`);
      const descriptor = await readFile(descriptorPath);
      await client.script('await browser.unbind(); return true;');
      await assert.rejects(() => access(descriptorPath));
      // A stale descriptor forces the client to try the old socket, proving it was closed.
      await writeFile(descriptorPath, descriptor);
      const closed = await run(['run', '--session', name, '--eval', 'return true;', '--json', '--no-inherit'], { env });
      assert.notEqual(closed.code, 0);
      assert.match(closed.text, /is not reachable/);
    } finally { await client.close(); }
  });
}

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: cooperative close retires a global binding before its ID and port are reused`, async () => {
    const client = await McpClient.launch(backend);
    const env = { FERRIDRIVER_SESSION_DIR: client.registry };
    const name = `mcp-close-${crypto.randomUUID()}`;
    try {
      ok(await client.call('navigate', { url: dataUrl('<h1>first owner</h1>') }));
      const endpoint = await client.script("return (await browser.bind(args[0], { host: '127.0.0.1', port: 0 })).endpoint;", [name]);
      const duplicate = await client.script("try { await browser.bind(args[0]); return 'accepted'; } catch (error) { return String(error); }", [`${name}-duplicate`]);
      assert.match(duplicate, /already has a bound session/);
      passed(await run(['session', 'close', name, '--no-inherit'], { env }));
      await assert.rejects(() => access(join(client.registry, `${name}.json`)));
      ok(await client.call('navigate', { url: dataUrl('<h1>replacement owner</h1>') }));
      const replacement = await client.script("return (await browser.bind(args[0], { host: '127.0.0.1', port: args[1] })).endpoint;", [name, Number(new URL(endpoint).port)]);
      assert.equal(replacement, endpoint);
      const result = await run(['run', '--session', name, '--eval', 'return await page.snapshotForAI();', '--json', '--no-inherit'], { env });
      passed(result);
      assert.match(result.stdout, /replacement owner/);
      await client.script('await browser.unbind(); return true;');
    } finally { await client.close(); }
  });
}
