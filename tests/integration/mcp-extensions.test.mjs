import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, dataUrl, isError, ok, payload } from './mcp-client.mjs';

async function clientWithTools() {
  const extension = resolve('tests/integration/fixtures/mcp-tools.js');
  const root = await workspace({ 'ferridriver.toml': `extensions = [${JSON.stringify(extension)}]\n` });
  return McpClient.launch('cdp-pipe', `${root}/ferridriver.toml`);
}

test('MCP promoted extensions expose metadata, enforce schemas and preserve session routing', async () => {
  const client = await clientWithTools();
  try {
    const tools = ok(await client.request('tools/list', {})).result.tools;
    const typed = tools.find(tool => tool.name === 'typed_greet');
    assert.ok(typed);
    assert.equal(typed.title, 'Typed Greeter');
    assert.equal(typed.annotations.readOnlyHint, true);
    assert.equal(typed.annotations.openWorldHint, false);
    assert.equal(typed.inputSchema.required[0], 'user');
    assert.equal(typed.outputSchema.required[0], 'greeting');
    assert.ok(tools.some(tool => tool.name === 'bad_output'));
    assert.equal(tools.some(tool => tool.name === 'cap_register'), false);
    const good = ok(await client.call('typed_greet', { user: 'sashoush', session: 'default' }));
    assert.equal(good.result.structuredContent.greeting, 'hi sashoush');
    assert.match(good.result.content[0].text, /hi sashoush/);
    const missing = await client.call('typed_greet');
    assert.equal(isError(missing), true);
    assert.match(missing.result.content[0].text, /invalid arguments/);
    assert.equal(isError(await client.call('typed_greet', { user: 'sashoush', extra: 1 })), true);
    const bad = await client.call('bad_output');
    assert.equal(isError(bad), true);
    assert.match(bad.result.content[0].text, /outputSchema/);
    const intro = JSON.parse(ok(await client.call('ferridriver_extensions')).result.content[0].text);
    assert.equal(intro.count, 3);
    assert.equal(intro.files[0].tools[0].name, 'typed_greet');
    assert.deepEqual(intro.errors, []);
    assert.deepEqual(intro.warnings, []);
  } finally { await client.close(); }
});

test('MCP extension fetch permissions follow event, route and exposed-function callbacks', async () => {
  const client = await clientWithTools();
  try {
    ok(await client.call('navigate', { url: dataUrl('<!doctype html><title>cap</title><body>cap</body>') }));
    assert.equal(await client.script("return await tools['cap_register']();"), 'registered');
    await client.script(`
      await page.exposeFunction('__ctlProbe', async () => {
        try { await fetch('http://blocked.test/'); globalThis.__cap.control = 'ALLOWED'; }
        catch (error) { globalThis.__cap.control = String(error?.message || error); }
        return 1;
      });
      return true;
    `);
    const result = payload(ok(await client.call('run_script', { source: `
      await page.evaluate("console.log('cap-console')");
      await page.evaluate("fetch('http://ferri.invalid/cap-route').catch(() => {})");
      await page.evaluate('window.__capProbe()');
      await page.evaluate('window.__ctlProbe()');
      await globalThis.__capDone;
      return globalThis.__cap;
    `, timeout_ms: 45000 })));
    assert.equal(result.status, 'ok');
    for (const key of ['pageOn', 'route', 'exposeFn']) {
      assert.equal(typeof result.value[key], 'string', key);
      assert.match(result.value[key], /permission denied/);
      assert.match(result.value[key], /blocked.test/);
    }
    for (const key of ['globalFetch', 'control']) {
      assert.equal(typeof result.value[key], 'string', key);
      assert.doesNotMatch(result.value[key], /permission denied/);
    }
  } finally { await client.close(); }
});
