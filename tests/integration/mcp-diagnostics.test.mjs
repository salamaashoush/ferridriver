import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';

const urls = [
  'https://cdn.example.com/assets/main.js', 'https://localhost:3000/remote.js',
  ...['one', 'two', 'three', 'four'].map(path => `https://app.example.com/api/${path}`),
];

test('MCP diagnostics filter before limiting, preserve order and honor case and severity', async () => {
  const client = await McpClient.launch();
  try {
    ok(await client.call('navigate', { url: dataUrl('<body>diagnostics</body>') }));
    await client.script(`
      const received = page.waitForEvent('console', { predicate: message => message.text() === args[0].at(-1) });
      await page.evaluate(urls => urls.forEach(url => console.log(url)), args[0]);
      await received;
      return true;
    `, [urls]);
    const read = async args => JSON.parse(ok(await client.call('diagnostics', { type: 'console', ...args })).result.content[0].text);
    const select = async args => (await read(args)).map(message => message.text);
    assert.deepEqual(await select({ limit: 2 }), urls.slice(-2));
    assert.deepEqual(await select({ filter: 'cdn.example', limit: 2 }), [urls[0]]);
    assert.deepEqual(await select({ filter: 'localhost:3000', limit: 2 }), [urls[1]]);
    assert.deepEqual(await select({ filter: 'CDN.EXAMPLE', limit: 5 }), [urls[0]]);
    assert.deepEqual(await select({ filter: 'nonesuch', limit: 50 }), []);
    assert.deepEqual(await select({ limit: 0 }), []);
    assert.deepEqual(await select({ limit: 100 }), urls);
    await client.script(`
      const received = page.waitForEvent('console', { predicate: message => message.text() === 'severity-error' });
      await page.evaluate(() => { console.warn('severity-warn'); console.error('severity-error'); });
      await received;
      return true;
    `);
    assert.deepEqual(await select({ level: 'warn' }), ['severity-warn'], JSON.stringify(await read({})));
    assert.ok((await read({})).some(message => message.type === 'error'));
  } finally { await client.close(); }
});
