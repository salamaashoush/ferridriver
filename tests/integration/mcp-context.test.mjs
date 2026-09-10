import assert from 'node:assert/strict';
import { stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';

test('MCP extension context carries session identity, settings, capabilities and durable variables', async () => {
  const root = await workspace({ 'ferridriver.toml': `
[extensions]
paths = [${JSON.stringify(resolve('tests/integration/fixtures/ctxprobe.js'))}]
[extensions.settings.ctxprobe]
env = "staging"
origin = "https://example.test"
[mcp.browser]
headless = true
[mcp.browser.instances.staging]
args = []
` });
  const client = await McpClient.launch('cdp-pipe', join(root, 'ferridriver.toml'));
  try {
    const response = ok(await client.call('ctxprobe.surface', { session: 'staging:admin' }));
    const output = response.result.structuredContent ?? JSON.parse(response.result.content.at(-1).text);
    const value = output.value ?? output;
    for (const [key, expected] of Object.entries({
      sessionKey: 'staging:admin', instance: 'staging', contextName: 'admin',
      settingsEnv: 'staging', settingsOrigin: 'https://example.test',
      errorEnabled: true, bogusLevelEnabled: false,
    })) assert.equal(value[key], expected, key);
    assert.deepEqual(value.logLevels, ['error', 'warn', 'info', 'debug', 'trace']);
    for (const key of ['hasVars', 'hasEnabled', 'hasFs', 'hasArtifacts', 'hasSidecars', 'hasLog', 'hasCommands', 'hasPage']) {
      assert.equal(value[key], true, key);
    }
    ok(await client.call('ctxprobe.remember', { value: 'abc123', session: 'staging:admin' }));
    assert.match(JSON.stringify(ok(await client.call('ctxprobe.recall', { session: 'staging:admin' })).result), /abc123/);
  } finally { await client.close(); }
});

test('MCP instance keys select configured profiles and share their browser across contexts', async () => {
  const root = await workspace({});
  const config = join(root, 'ferridriver.toml');
  await writeFile(config, `
[mcp.browser]
headless = true
chromeArgs = ["--base-flag"]
[mcp.browser.instances.staging]
userDataDir = ${JSON.stringify(join(root, 'profiles', '${INSTANCE}'))}
args = ["--window-size=900,700"]
[mcp.browser.instances.other]
args = []
`);
  const client = await McpClient.launch('cdp-pipe', config);
  try {
    ok(await client.call('navigate', { url: 'about:blank', session: 'staging' }));
    assert.equal((await stat(join(root, 'profiles/staging/Default'))).isDirectory(), true);
    ok(await client.call('evaluate', { expression: '1 + 1', session: 'staging' }));
    const unknown = await client.call('navigate', { url: 'about:blank', session: 'typo-env:admin' });
    assert.equal(unknown.result.isError, true);
    assert.match(unknown.result.content[0].text, /typo-env/);
    assert.match(unknown.result.content[0].text, /staging/);
    for (const context of ['admin', 'tester']) {
      ok(await client.call('navigate', { url: 'about:blank', session: `staging:${context}` }));
    }
  } finally { await client.close(); }
});

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP can create, list and select a second page`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: dataUrl('<body>seed</body>') }));
      ok(await client.call('page', { action: 'new' }));
      const listed = ok(await client.call('page', { action: 'list' }));
      assert.match(listed.result.content[0].text, /Page 0/);
      assert.match(listed.result.content[0].text, /Page 1/);
      ok(await client.call('page', { action: 'select', page_index: 0 }));
    } finally { await client.close(); }
  });
}
