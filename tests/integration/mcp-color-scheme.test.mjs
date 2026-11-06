import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';

// An MCP session's page is emulated light like any other page whose
// context names no colour scheme, whatever the host's appearance.
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: an MCP session page is emulated light`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: dataUrl('<p>text</p>') }));
      const response = ok(await client.call('evaluate', {
        expression: "JSON.stringify([matchMedia('(prefers-color-scheme: light)').matches, matchMedia('(prefers-color-scheme: dark)').matches])",
      }));
      assert.match(response.result.content[0].text, /\[true,false\]/);
    } finally { await client.close(); }
  });
}
