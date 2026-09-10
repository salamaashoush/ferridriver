import assert from 'node:assert/strict';
import { writeFile, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, isError, ok } from './mcp-client.mjs';

test('MCP artifact resources preserve bytes, infer MIME types and reject traversal', async () => {
  const entries = [
    ['screenshots/a.png', 'image/png'], ['x.PDF', 'application/pdf'], ['run.trace', 'application/json'],
    ['notes.txt', 'text/plain'], ['blob.bin', 'application/octet-stream'], ['noext', 'application/octet-stream'],
  ];
  const root = await workspace(Object.fromEntries(entries.map(([name]) => [`artifacts/${name}`, `content of ${name}`])));
  await writeFile(join(root, 'secret'), 'must not be served');
  await symlink(join(root, 'secret'), join(root, 'artifacts/escape'));
  const config = join(root, 'ferridriver.toml');
  await writeFile(config, `artifactsRoot = ${JSON.stringify(join(root, 'artifacts'))}\n`);
  const client = await McpClient.launch('cdp-pipe', config);
  try {
    const resources = ok(await client.request('resources/list', {})).result.resources;
    for (const [name, mime] of entries) {
      const uri = `artifact://${name}`;
      assert.ok(resources.some(resource => resource.uri === uri && resource.mimeType === mime));
      const content = ok(await client.request('resources/read', { uri })).result.contents[0];
      assert.equal(content.mimeType, mime);
      const value = mime.startsWith('text/') || mime === 'application/json' ? content.text : atob(content.blob);
      assert.equal(value, `content of ${name}`);
    }
    const escaped = await client.request('resources/read', { uri: 'artifact://../secret' });
    assert.equal(isError(escaped), true, JSON.stringify(escaped));
    assert.equal(isError(await client.request('resources/read', { uri: 'artifact://escape' })), true);
  } finally { await client.close(); }
});
