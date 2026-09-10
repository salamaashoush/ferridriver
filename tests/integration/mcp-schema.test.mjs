import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, isError, ok } from './mcp-client.mjs';
import { workspace } from './support.mjs';

test('MCP discovery defaults to stable and forwards explicit Chrome channels', async () => {
  const client = await McpClient.launch();
  const user_data_dir = await workspace({});
  try {
    for (const channel of [undefined, 'stable', 'beta', 'canary']) {
      const result = await client.call('connect', { auto_discover: true, user_data_dir, ...(channel ? { channel } : {}) });
      assert.equal(isError(result), true);
      const text = JSON.stringify(result);
      assert.ok(text.includes(`Chrome (${channel ?? 'stable'})`), text);
      assert.ok(text.includes(`${user_data_dir}/DevToolsActivePort`), text);
    }
  } finally { await client.close(); }
});

test('MCP schemas enumerate legal arguments and publish structured result contracts', async () => {
  const client = await McpClient.launch();
  try {
    const tools = ok(await client.request('tools/list', {})).result.tools;
    const find = name => { const tool = tools.find(tool => tool.name === name); assert.ok(tool, name); return tool; };
    for (const [name, terms] of [
      ['page', ['back', 'forward', 'reload', 'new', 'close', 'select', 'list', 'close_context', 'close_instance', 'close_browser']],
      ['screenshot', ['png', 'jpeg', 'webp', 'minimum', 'maximum']],
      ['diagnostics', ['console', 'network', 'trace_start', 'trace_stop']],
    ]) {
      const schema = JSON.stringify(find(name).inputSchema);
      for (const term of terms) assert.ok(schema.includes(term), `${name}: ${term}`);
    }
    for (const [name, term] of [['run_script', 'duration_ms'], ['run_bdd', 'scenarios']]) {
      assert.ok(JSON.stringify(find(name).outputSchema).includes(term));
    }
    assert.equal(find('snapshot').annotations.openWorldHint, false);
    assert.equal(find('run_script').title, 'Run Browser Script');
    assert.equal(find('run_script').annotations.readOnlyHint, false);
    for (const [name, args, terms] of [
      ['page', { action: 'close_tab' }, ['close_context']],
      ['diagnostics', { type: 'netwrok' }, ['console', 'network', 'trace_start', 'trace_stop']],
      ['diagnostics', {}, ['type']],
    ]) {
      const response = await client.call(name, args);
      assert.equal(isError(response), true);
      for (const term of terms) assert.ok(JSON.stringify(response).includes(term), term);
    }
  } finally { await client.close(); }
});

test('MCP accepts the jpg alias and returns JPEG bytes and matching resource metadata', async () => {
  const client = await McpClient.launch();
  try {
    ok(await client.call('navigate', { url: dataUrl('<h1>jpeg</h1>') }));
    const content = ok(await client.call('screenshot', { format: 'jpg' })).result.content;
    const image = content.find(block => block.type === 'image');
    assert.equal(image.mimeType, 'image/jpeg');
    assert.match(image.data, /^\/9j\//);
    const link = content.find(block => block.type === 'resource_link');
    assert.equal(link.mimeType, 'image/jpeg');
    assert.match(link.uri, /\.jpg$/);
  } finally { await client.close(); }
});
