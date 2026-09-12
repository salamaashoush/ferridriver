import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient } from './mcp-client.mjs';

test('route fulfillment cannot read files after the realm revokes read permission', async () => {
  const client = await McpClient.launch();
  try {
    const result = await client.script(`
      process.permission.drop('read');
      let denied;
      await page.route('http://example.test/private', async route => {
        try { await route.fulfill({ path: '/etc/hosts' }); }
        catch (error) {
          denied = { name: error.name, code: error.code };
          await route.fulfill({ body: '<title>refused</title>', contentType: 'text/html' });
        }
      });
      await page.goto('http://example.test/private');
      return { denied, title: await page.title() };
    `);
    assert.deepEqual(result, {
      denied: { name: 'PermissionDeniedError', code: 'ERR_ACCESS_DENIED' }, title: 'refused',
    });
  } finally { await client.close(); }
});
