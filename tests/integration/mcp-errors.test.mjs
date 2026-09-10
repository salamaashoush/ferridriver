import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';

function toolError(response) {
  assert.equal(Object.hasOwn(response, 'error'), false, JSON.stringify(response));
  assert.equal(response.result.isError, true);
  assert.ok(response.result.content[0].text.trim().length > 0);
  return response.result.content.filter(block => block.type === 'text').map(block => block.text).join('\n');
}

test('MCP distinguishes protocol errors from failed operations and keeps serving after both', async () => {
  const client = await McpClient.launch();
  try {
    toolError(await client.call('navigate', { url: 'http://127.0.0.1:1/nope' }));
    assert.match(toolError(await client.call('page', { action: 'definitely-not-an-action' })), /close_browser/);
    toolError(await client.call('page', { action: 'select' }));
    toolError(await client.call('page', { action: 'select', page_index: 99 }));
    ok(await client.call('navigate', { url: dataUrl('<h1>err</h1>') }));
    const thrown = toolError(await client.call('run_script', { source: "throw new Error('boom');" }));
    assert.match(thrown, /"status": "error"/);
    assert.match(thrown, /boom/);
    assert.match(thrown, /\[runtime_error\]/);
    ok(await client.call('navigate', { url: dataUrl('<h1>fine</h1>') }));
    ok(await client.call('run_script', { source: 'return 1 + 1;' }));
    ok(await client.call('search_page', { pattern: 'nothing-matches-this-string-xyzzy' }));
    const unknown = await client.call('no_such_tool_at_all');
    assert.ok(Object.hasOwn(unknown, 'error'));
    ok(await client.call('navigate', { url: dataUrl('<h1>after</h1>') }));
    const after = ok(await client.call('evaluate', { expression: 'document.title' }));
    assert.ok(after.result.content[0].text.length > 0);
  } finally { await client.close(); }
});
