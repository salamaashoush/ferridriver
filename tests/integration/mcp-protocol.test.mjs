import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, ok } from './mcp-client.mjs';

test('MCP advertises titles and accurate observer, navigation and tab annotations', async () => {
  const client = await McpClient.launch();
  try {
    const response = ok(await client.request('tools/list', {}));
    const tools = response.result.tools;
    const find = name => {
      const tool = tools.find(tool => tool.name === name);
      assert.ok(tool, `missing ${name}`);
      return tool;
    };
    assert.equal(find('snapshot').title, 'Accessibility Snapshot');
    assert.equal(find('snapshot').annotations.readOnlyHint, true);
    assert.equal(find('navigate').title, 'Navigate');
    assert.equal(find('navigate').annotations.openWorldHint, true);
    assert.equal(find('navigate').annotations.readOnlyHint, false);
    assert.equal(find('page').annotations.destructiveHint, true);
    for (const tool of tools) assert.equal(typeof tool.title, 'string', tool.name);
  } finally { await client.close(); }
});

test('MCP rejects an unknown method and continues serving the same session', async () => {
  const client = await McpClient.launch();
  try {
    const response = await client.request('missing-method', {});
    assert.equal(response.error.code, -32601);
    assert.deepEqual(ok(await client.request('ping', {})).result, {});
  } finally { await client.close(); }
});
