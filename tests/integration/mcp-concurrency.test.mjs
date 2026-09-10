import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, ok, payload } from './mcp-client.mjs';

test('concurrent MCP callers each receive their own correlated response', async () => {
  const client = await McpClient.launch();
  try {
    const replies = await Promise.all(Array.from({ length: 32 }, (_, index) =>
      client.call('run_script', { source: 'return args[0]', args: [index] })));
    assert.deepEqual(replies.map(reply => payload(ok(reply)).value), Array.from({ length: 32 }, (_, index) => index));
    assert.equal(new Set(replies.map(reply => reply.id)).size, 32);
  } finally {
    await client.close();
  }
});
