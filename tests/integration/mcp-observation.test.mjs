import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';
import { fixtureServer } from './fixture-server.mjs';

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP snapshots, screenshots, search and diagnostics describe the page`, async () => {
    const client = await McpClient.launch(backend);
    const server = await fixtureServer();
    const navigate = html => client.call('navigate', { url: dataUrl(html) }).then(ok);
    const text = async (name, args = {}) => ok(await client.call(name, args)).result.content[0].text;
    try {
      await navigate('<h1>Snap</h1><button>Click</button>');
      const snapshot = await text('snapshot');
      assert.match(snapshot, /\[ref=/);
      assert.match(snapshot, /Snap/);
      await navigate('<div style="height:3000px">tall</div>');
      await client.script("await page.evaluate('window.scrollBy(0, 500)'); return null;");
      assert.match(await text('snapshot'), /Scroll:/);
      await navigate('<h1>Screenshot</h1>');
      await client.script("await page.waitForSelector('h1'); return true;");
      const shot = ok(await client.call('screenshot')).result.content.find(block => block.type === 'image');
      assert.match(shot.data, /^iVBOR/);
      await navigate('<div style="height:3000px">tall</div>');
      const full = ok(await client.call('screenshot', { full_page: true })).result.content.find(block => block.type === 'image');
      assert.match(full.data, /^iVBOR/);
      assert.ok(full.data.length > 1000);
      await navigate('<p>Alpha Beta Gamma</p><p>Delta Beta Epsilon</p>');
      const found = await text('search_page', { pattern: 'Beta' });
      assert.match(found, /2/);
      assert.match(found, /Beta/);
      await navigate('<p>Order #123</p><p>Order #456</p>');
      assert.match(await text('search_page', { pattern: 'Order #\\d+', regex: true }), /2/);
      await navigate('<p>Hello world</p>');
      assert.match(await text('search_page', { pattern: 'nonexistent' }), /No matches|0/);
      await client.script("globalThis.consoleSeen = page.waitForEvent('console', { predicate: message => message.text() === 'warn456' });");
      await text('evaluate', { expression: "console.log('hello123'); console.warn('warn456')" });
      await client.script('await globalThis.consoleSeen;');
      const consoleOutput = await text('diagnostics', { type: 'console' });
      assert.match(consoleOutput, /hello123/);
      assert.match(consoleOutput, /warn456/);
      ok(await client.call('navigate', { url: `${server.url}/fx/har` }));
      assert.match(await text('diagnostics', { type: 'network' }), /127\.0\.0\.1|GET|request/);
      if (backend.startsWith('cdp')) {
        await navigate('<body></body>');
        await text('diagnostics', { type: 'trace_start' });
        await text('evaluate', { expression: 'for(let i=0;i<1000;i++) Math.sqrt(i)' });
        const trace = await text('diagnostics', { type: 'trace_stop' });
        for (const expected of ['Trace stopped', 'Requests:', 'Insights']) assert.ok(trace.includes(expected));
      }
    } finally {
      await client.close();
      await server.close();
    }
  });
}
