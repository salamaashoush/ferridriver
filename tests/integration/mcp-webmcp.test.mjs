import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, dataUrl, isError, ok } from './mcp-client.mjs';

const text = response => (response.result?.content ?? [])
  .filter(block => block.type === 'text').map(block => block.text).join('\n')
  || JSON.stringify(response.error ?? response);

const page = `<!doctype html><title>Tools</title><output id=result></output>
<iframe name="side" src="http://127.0.0.1:47839/side"></iframe>
<script>
  const model = document.modelContext;
  model.registerTool({name: 'set_message', title: 'Set message', description: 'Updates the message',
    inputSchema: {type: 'object', properties: {message: {type: 'string'}}, required: ['message']},
    annotations: {readOnlyHint: true},
    execute: async ({message}) => { document.querySelector('#result').textContent = message; return {message}; }});
  model.registerTool({name: 'fails', description: 'Throws', execute: async () => { throw new Error('sashoush refused'); }});
  model.registerTool({name: 'waits', description: 'Waits for its signal', execute: (_input, {signal}) =>
    new Promise(() => signal.addEventListener('abort', () => { window.canceled = signal.reason?.name; }))});
</script>`;
const side = `<!doctype html><script>
  document.modelContext.registerTool({name: 'side_tool', description: 'Lives in the side frame',
    execute: async input => ({frame: 'side', input})});
</script>`;

test('MCP lists a page\'s WebMCP tools on navigation and calls them by name', async () => {
  const root = await workspace({ 'ferridriver.toml': '[mcp.browser]\nchromeArgs = ["--enable-features=WebMCP"]\n' });
  const client = await McpClient.launch('cdp-pipe', join(root, 'ferridriver.toml'));
  try {
    await client.script(`
      await page.route('http://127.0.0.1:47839/side', route => route.fulfill({contentType: 'text/html', body: ${JSON.stringify(side)}}));
      await page.route('http://127.0.0.1:47839/', route => route.fulfill({contentType: 'text/html', body: ${JSON.stringify(page)}}));
      return true;`);
    const navigated = text(ok(await client.call('navigate', { url: 'http://127.0.0.1:47839/' })));
    assert.match(navigated, /### WebMCP tools \(3\)/);
    assert.match(navigated, /- set_message \(Set message\): Updates the message/);
    assert.match(navigated, /hints: read-only/);
    assert.doesNotMatch(navigated, /side_tool/, 'the navigation reply lists only the main frame');

    const listed = text(ok(await client.call('webmcp_tools')));
    assert.match(listed, /### WebMCP tools \(4\)/);
    assert.match(listed, /- side_tool: Lives in the side frame\n {2}frame named "side": http:\/\/127\.0\.0\.1:47839\/side/);

    assert.deepEqual(JSON.parse(text(ok(await client.call('webmcp_call', {
      name: 'set_message', input: { message: 'sashoush' },
    })))), { message: 'sashoush' });
    assert.equal(await client.script(`return await page.locator('#result').textContent();`), 'sashoush');
    assert.deepEqual(JSON.parse(text(ok(await client.call('webmcp_call', {
      name: 'side_tool', input: { value: 1 },
    })))), { frame: 'side', input: { value: 1 } });
    assert.deepEqual(JSON.parse(text(ok(await client.call('webmcp_call', {
      name: 'side_tool', frame: 'side',
    })))), { frame: 'side', input: {} });

    const failed = await client.call('webmcp_call', { name: 'fails' });
    assert.ok(isError(failed));
    assert.match(text(failed), /WebMCP tool "fails" failed: Error: sashoush refused/);
    const missing = await client.call('webmcp_call', { name: 'missing' });
    assert.ok(isError(missing));
    assert.match(text(missing), /No WebMCP tool named "missing"\. Available tools: /);
    const timedOut = await client.call('webmcp_call', { name: 'waits', timeout: 200 });
    assert.ok(isError(timedOut));
    assert.match(text(timedOut), /Timeout 200ms exceeded/);
    assert.equal(await client.script(`
      await page.waitForFunction(() => window.canceled);
      return await page.evaluate(() => window.canceled);`), 'AbortError');

    const plain = text(ok(await client.call('navigate', { url: dataUrl('<h1>No tools</h1>') })));
    assert.doesNotMatch(plain, /WebMCP/);
  } finally { await client.close(); }
});
