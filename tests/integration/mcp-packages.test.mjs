import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, ok } from './mcp-client.mjs';

const settingsSchema = { type: 'object', properties: { origin: { type: 'string' } }, required: ['origin'], additionalProperties: false };
const sign = `import { SHARED_MARKER } from './lib/shared';
defineTool({ name: 'pkgprobe.sign', description: 'Entry two', exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} }, async handler() { return { marker: SHARED_MARKER }; } });`;
async function fixture(manifest = {}, settings = '') {
  const root = await workspace({
    'pkgprobe/package.json': JSON.stringify({ name: '@probe/pkgprobe', type: 'module', ferridriver: {
      entries: ['./src/globals.ts', './src/login.ts', './src/sign.ts'], ...manifest,
    } }),
    'pkgprobe/src/lib/shared.ts': "export const SHARED_MARKER = 'from-lib';",
    'pkgprobe/src/globals.ts': "globalThis.pkgprobeGlobal = 'installed-by-globals-entry';",
    'pkgprobe/src/login.ts': `import { SHARED_MARKER } from './lib/shared';
defineTool({ name: 'pkgprobe.login', description: 'Entry one', exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} }, async handler({ settings }) {
    return { marker: SHARED_MARKER, origin: settings?.origin ?? null, fromGlobalsEntry: globalThis.pkgprobeGlobal ?? null };
  } });`,
    'pkgprobe/src/sign.ts': sign,
  });
  const config = join(root, 'ferridriver.toml');
  await writeFile(config, `[extensions]\npaths = [${JSON.stringify(join(root, 'pkgprobe'))}]\n${settings}\n[mcp.browser]\nheadless = true\n`);
  return { root, client: await McpClient.launch('cdp-pipe', config) };
}
async function extensions(client, args = {}) {
  return JSON.parse(ok(await client.call('ferridriver_extensions', args)).result.content[0].text);
}

test('MCP packages load every declared entry and bundle helpers without treating them as extensions', async () => {
  const { client } = await fixture({ requires: { commands: ['sh'] }, settings: { pkgprobe: settingsSchema } },
    '[extensions.settings.pkgprobe]\norigin = "https://probe.test"\n');
  try {
    const result = await extensions(client);
    assert.deepEqual(result.errors, []);
    assert.equal(result.count, 2);
    assert.equal(result.files.length, 3);
    assert.match(JSON.stringify(result.warnings), /globals.ts/);
    assert.match(JSON.stringify(result.warnings), /declares no tools/);
    for (const file of result.files) assert.doesNotMatch(file.path, /\/lib\//);
    for (const name of ['pkgprobe.login', 'pkgprobe.sign']) {
      assert.match(JSON.stringify(ok(await client.call(name)).result), /from-lib/);
    }
    const login = JSON.stringify(ok(await client.call('pkgprobe.login')).result);
    assert.match(login, /https:\/\/probe.test/);
    assert.match(login, /installed-by-globals-entry/);
  } finally { await client.close(); }
});

for (const [name, manifest, settings, expected] of [
  ['missing sidecar', { requires: { sidecars: ['never-declared-gate'] } }, '', ['never-declared-gate', '[[sidecars]]']],
  ['invalid settings', { settings: { pkgprobe: settingsSchema } }, '[extensions.settings.pkgprobe]\norigins = "https://probe.test"\n', ['extensions.settings.pkgprobe']],
]) {
  test(`MCP refuses a package with ${name} and names the configuration to fix`, async () => {
    const { client } = await fixture(manifest, settings);
    try {
      const result = await extensions(client);
      assert.equal(result.count, 0);
      for (const text of expected) assert.ok(JSON.stringify(result.errors).includes(text));
    } finally { await client.close(); }
  });
}

test('MCP reload updates advertised tools and code in a live session', async () => {
  const { root, client } = await fixture();
  try {
    const session = 'default:reload';
    assert.match(JSON.stringify(ok(await client.call('pkgprobe.login', { session })).result), /from-lib/);
    const before = ok(await client.request('tools/list', {})).result.tools.map(tool => tool.name);
    assert.ok(before.includes('pkgprobe.login'));
    assert.equal(before.includes('pkgprobe.extra'), false);
    await writeFile(join(root, 'pkgprobe/src/lib/shared.ts'), "export const SHARED_MARKER = 'edited-lib';");
    await writeFile(join(root, 'pkgprobe/src/sign.ts'), sign + `
defineTool({ name: 'pkgprobe.extra', description: 'Added by the edit', exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} }, async handler() { return { added: true }; } });`);
    const result = await extensions(client, { action: 'reload' });
    assert.equal(result.count, 3);
    assert.deepEqual(result.reloaded.added, ['pkgprobe.extra']);
    assert.equal(result.reloaded.toolListChanged, true);
    assert.ok(result.reloaded.droppedSessionVms >= 1);
    assert.ok(ok(await client.request('tools/list', {})).result.tools.some(tool => tool.name === 'pkgprobe.extra'));
    assert.match(JSON.stringify(ok(await client.call('pkgprobe.extra', { session })).result), /added/);
    assert.match(JSON.stringify(ok(await client.call('pkgprobe.login', { session })).result), /edited-lib/);
  } finally { await client.close(); }
});
