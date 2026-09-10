import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, ok } from './mcp-client.mjs';

test('MCP propagates valid trace parents into tool spans and rejects malformed identifiers', async () => {
  const root = await workspace({ 'tool.js': `defineTool({ name: 'trace_probe', exposeAsTool: true,
    handler: ({ args, log }) => { log.warn('trace-probe-' + args.index); return true; } });` });
  const config = join(root, 'ferridriver.toml');
  await writeFile(config, `extensions = [${JSON.stringify(join(root, 'tool.js'))}]\n`);
  const client = await McpClient.launch('cdp-pipe', config, { log: 'info' });
  try {
    const parents = [
      '00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',
      '00-00000000000000000000000000000000-00f067aa0ba902b7-01',
      '00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01',
      'garbage', '00-tooShort-00f067aa0ba902b7-01',
      '00-ZZf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01',
    ];
    for (const [index, traceparent] of parents.entries()) {
      ok(await client.request('tools/call', { name: 'trace_probe', arguments: { index }, _meta: { traceparent } }));
    }
    await commands.write('mcp', null);
    assert.equal(await commands.wait('mcp', 2000), 0);
    const { stderr } = await commands.status('mcp');
    for (const [index] of parents.entries()) {
      const line = stderr.split('\n').find(line => line.includes(`trace-probe-${index}`));
      assert.ok(line, stderr);
      if (index === 0) {
        assert.match(line, /trace_id="?4bf92f3577b34da6a3ce929d0e0e4736/);
        assert.match(line, /parent_span_id="?00f067aa0ba902b7/);
      } else {
        assert.doesNotMatch(line, /trace_id=|parent_span_id=/);
      }
    }
  } finally { await client.close(); }
});
