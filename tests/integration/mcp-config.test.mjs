import assert from 'node:assert/strict';
import { stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, ok, payload } from './mcp-client.mjs';

test('MCP initialization applies the configured server identity and instructions', async () => {
  const root = await workspace({ 'ferridriver.toml': `[mcp.server]
name = "sashoush-browser"
instructions = "Configured instructions"
extraInstructions = "Ignored extra instructions"
` });
  const client = await McpClient.launch('cdp-pipe', join(root, 'ferridriver.toml'));
  try {
    assert.equal(client.initialized.result.serverInfo.name, 'sashoush-browser');
    assert.ok(client.initialized.result.instructions.startsWith('Configured instructions'));
    assert.equal(client.initialized.result.instructions.includes('Ignored extra instructions'), false);
  } finally { await client.close(); }
});

test('MCP section arguments reach Chrome once and profile placeholders expand for each instance', async () => {
  const root = await workspace({});
  const config = join(root, 'ferridriver.toml');
  await writeFile(config, `[mcp.browser]
chromeArgs = ["--host-resolver-rules=MAP a 1.1.1.1"]
userDataDir = ${JSON.stringify(join(root, 'profiles', '${INSTANCE}'))}
[mcp.browser.instances.staging]
args = ["--staging-flag"]
[mcp.browser.instances.dev]
args = []
`);
  const client = await McpClient.launch('cdp-pipe', config);
  try {
    for (const session of ['staging', 'dev']) {
      const result = payload(ok(await client.call('run_script', { session, source: `
        await page.goto('about:blank');
        const cdp = await context.newCDPSession(page);
        try { return (await cdp.send('Browser.getBrowserCommandLine', {})).arguments; }
        finally { await cdp.detach(); }
      ` })));
      assert.equal(result.status, 'ok');
      const args = result.value;
      assert.equal(args.filter(arg => arg.startsWith('--host-resolver-rules')).length, 1);
      assert.equal(args.filter(arg => arg === '--staging-flag').length, session === 'staging' ? 1 : 0);
      assert.ok(args.includes(`--user-data-dir=${join(root, 'profiles', session)}`), JSON.stringify(args));
      assert.equal((await stat(join(root, 'profiles', session, 'Default'))).isDirectory(), true);
    }
  } finally { await client.close(); }
});
