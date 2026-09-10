import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, isError, ok } from './mcp-client.mjs';

const values = [
  ['number', '1 + 1', /2/],
  ['string', "'hello'", /hello/],
  ['DOM', "document.querySelector('h1').textContent", /Test/],
  ['promise', 'Promise.resolve(42)', /42/],
  ['boolean', 'true', /true/],
  ['array', 'JSON.stringify([1,2,3])', /1.*3/s],
  ['object', '({a: 1, b: true})', /a.*1/s],
  ['null', 'null', /null|undefined/],
];
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP evaluates primitives, DOM, promises and large payloads`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: dataUrl('<h1>Test</h1>') }));
      for (const [name, expression, expected] of values) {
        const response = ok(await client.call('evaluate', { expression }));
        assert.match(response.result.content[0].text, expected, name);
      }
      const large = ok(await client.call('evaluate', { expression: "JSON.stringify(Array(1000).fill('x'))" }));
      assert.ok(large.result.content[0].text.length > 1000);
      for (const expression of ['thisFunctionDoesNotExist()', 'function{']) {
        assert.equal(isError(await client.call('evaluate', { expression })), true, expression);
      }
    } finally { await client.close(); }
  });
}
