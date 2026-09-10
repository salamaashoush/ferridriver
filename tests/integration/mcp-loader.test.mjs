import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, isError, ok } from './mcp-client.mjs';

async function fixture(files, paths, extra = '') {
  const root = await workspace(files);
  const config = join(root, 'ferridriver.toml');
  await writeFile(config, `[extensions]\npaths = ${JSON.stringify(paths.map(path => path.startsWith('./') ? join(root, path) : path))}\n${extra}`);
  return { root, client: await McpClient.launch('cdp-pipe', config) };
}
const report = async (client, args = {}) => JSON.parse(ok(await client.call('ferridriver_extensions', args)).result.content[0].text);

test('MCP reports capabilities outside the operator policy and retains permitted tools', async () => {
  const { client } = await fixture({ 'tools.js': `
defineTool({ name: 'net', allow: { net: ['api.acme.com', 'evil.example'] }, handler: () => true });
defineTool({ name: 'argv', allow: { commands: { ok: { run: ['echo', 'hi'] } } }, handler: () => true });
` }, ['./tools.js'], '[extensions.policy]\nnet = ["*.acme.com"]\ncommands = "argvOnly"\n');
  try {
    const result = await report(client);
    assert.equal(result.count, 2);
    assert.deepEqual(result.errors, []);
    const warnings = JSON.stringify(result.warnings);
    assert.match(warnings, /evil.example/);
    assert.equal(warnings.includes('api.acme.com'), false);
  } finally { await client.close(); }
});

test('MCP rejects shell command declarations under an argv-only operator policy', async () => {
  await assert.rejects(() => fixture({ 'shell.js': `
defineTool({ name: 'shell', allow: { commands: { forbidden: 'echo hi' } }, handler: () => true });
` }, ['./shell.js'], '[extensions.policy]\ncommands = "argvOnly"\n'), error => {
    assert.match(error.message, /forbidden/);
    assert.match(error.message, /argvOnly/);
    return true;
  });
});

test('MCP loads explicit source files even with an unfamiliar extension', async () => {
  const { root, client } = await fixture({ 'tool.weird-ext': "defineTool({ name: 't', handler: () => 1 });" }, ['./tool.weird-ext']);
  try {
    const result = await report(client);
    assert.deepEqual(result.errors, []);
    assert.equal(result.count, 1);
    assert.deepEqual(result.files.map(file => file.path), [join(root, 'tool.weird-ext')]);
  } finally { await client.close(); }
});

test('MCP recursively discovers source entries while ignoring data and documentation', async () => {
  const { root, client } = await fixture({
    'entries/a.ts': "defineTool({ name: 'a', handler: () => 1 });",
    'entries/nested/b.tsx': "defineTool({ name: 'b', handler: () => 1 });",
    'entries/nested/deep/c.cjs': "defineTool({ name: 'c', handler: () => 1 });",
    'entries/nested/readme.md': 'this is not JavaScript', 'entries/data.json': '{}',
  }, ['./entries']);
  try {
    const result = await report(client);
    assert.deepEqual(result.errors, []);
    assert.deepEqual(result.files.map(file => file.path).sort(), ['entries/a.ts', 'entries/nested/b.tsx', 'entries/nested/deep/c.cjs'].map(path => join(root, path)));
  } finally { await client.close(); }
});

test('MCP records missing paths and packages while retaining resolvable entries', async () => {
  const { client } = await fixture({ 'ok.js': "defineTool({ name: 't', handler: () => 1 });" }, ['./ok.js', './missing/nope.js', 'no-such-package-xyz']);
  try {
    const result = await report(client);
    assert.equal(result.files.length, 1);
    assert.equal(result.errors.length, 2);
    assert.match(JSON.stringify(result.errors), /no-such-package-xyz/);
    assert.match(JSON.stringify(result.errors), /missing\/nope.js/);
  } finally { await client.close(); }
});

test('MCP isolates broken entries, runs entries without tools and exposes manifest metadata', async () => {
  const { client } = await fixture({
    'good.js': `defineTool({ name: 'good.tool', title: 'Good', exposeAsTool: true,
      annotations: { readOnlyHint: true }, outputSchema: { type: 'object' }, handler: () => ({ installed: globalThis.entryInstalled }) });`,
    'broken.js': 'this is not (valid js', 'empty.js': 'globalThis.entryInstalled = true;',
  }, ['./good.js', './broken.js', './empty.js']);
  try {
    const result = await report(client, { include_schema: true });
    assert.equal(result.files.length, 2);
    assert.equal(result.errors.length, 1);
    assert.match(result.errors[0].source, /broken.js$/);
    const empty = result.files.find(file => file.path.endsWith('/empty.js'));
    assert.deepEqual(empty.tools, []);
    const warning = result.warnings.find(warning => warning.source.endsWith('/empty.js'));
    assert.ok(warning);
    assert.match(warning.warning, /declares no tools/);
    const good = result.files.find(file => file.path.endsWith('/good.js')).tools[0];
    assert.equal(good.name, 'good.tool');
    assert.equal(good.title, 'Good');
    assert.equal(good.exposeAsMcpTool, true);
    assert.equal(good.outputSchema.type, 'object');
    assert.equal(good.annotations.readOnlyHint, true);
    assert.equal(ok(await client.call('good.tool')).result.structuredContent.installed, true);
  } finally { await client.close(); }
});

test('MCP introspection omits schemas by default and includes them on request', async () => {
  const { client } = await fixture({ 'acme-login.ts': `defineTool({
    name: 'acme.login', title: 'Acme Login', description: 'Logs in', exposeAsTool: true, timeoutMs: 5000,
    inputSchema: { type: 'object', properties: { user: { type: 'string' } } },
    outputSchema: { type: 'object', properties: { cookie: { type: 'string' } } },
    annotations: { readOnlyHint: false, destructiveHint: false },
    allow: { net: ['*.acme.com'], commands: { curlish: 'echo hi' } }, handler: () => ({ cookie: 'value' })
  });` }, ['./acme-login.ts']);
  try {
    const result = await report(client);
    assert.equal(result.count, 1);
    const tool = result.files[0].tools[0];
    for (const [key, value] of Object.entries({ name: 'acme.login', title: 'Acme Login', exposeAsMcpTool: true, timeoutMs: 5000 })) assert.equal(tool[key], value);
    assert.equal(tool.annotations.readOnlyHint, false);
    assert.deepEqual(tool.allow.net, ['*.acme.com']);
    assert.deepEqual(tool.allow.commands, ['curlish']);
    assert.equal(Object.hasOwn(tool, 'inputSchema'), false);
    assert.equal(Object.hasOwn(tool, 'outputSchema'), false);
    assert.deepEqual(result.warnings, []);
    const full = (await report(client, { include_schema: true })).files[0].tools[0];
    assert.equal(full.inputSchema.properties.user.type, 'string');
    assert.equal(full.outputSchema.properties.cookie.type, 'string');
  } finally { await client.close(); }
});

test('MCP rejects invalid arguments and validates structured outputs independently per tool', async () => {
  const definitions = [
    { name: 't', exposeAsTool: true, inputSchema: { type: 'object', properties: { user: { type: 'string' }, n: { type: 'integer' } }, required: ['user'], additionalProperties: false } },
    { name: 'mine', exposeAsTool: true, inputSchema: { type: 'object', properties: { session: { type: 'string', description: 'mine' } } } },
    { name: 'typed', exposeAsTool: true, outputSchema: { type: 'object', properties: { ok: { type: 'boolean' } }, required: ['ok'], additionalProperties: false } },
    { name: 'bad-output', exposeAsTool: true, outputSchema: { type: 'object', properties: { ok: { type: 'boolean' } }, required: ['ok'] } },
    { name: 'untyped', exposeAsTool: true },
  ];
  const source = definitions.map(definition => `defineTool({ ...${JSON.stringify(definition)}, handler: () => ${definition.name === 'bad-output' ? "({ ok: 'yes' })" : definition.name === 'untyped' ? "'anything'" : '({ ok: true })'} });`).join('\n');
  const { client } = await fixture({ 'tools.js': source }, ['./tools.js']);
  try {
    ok(await client.call('t', { user: 'sashoush', n: 3 }));
    for (const args of [{ n: 3 }, { user: 1 }, { user: 'sashoush', x: 1 }]) {
      const response = await client.call('t', args);
      assert.equal(isError(response), true);
      assert.ok(JSON.stringify(response).includes('invalid arguments for `t`'));
    }
    const tools = ok(await client.request('tools/list', {})).result.tools;
    const schema = tools.find(tool => tool.name === 't').inputSchema;
    assert.ok(schema.properties.session);
    assert.ok(schema.properties.user);
    assert.equal(schema.required.includes('session'), false);
    assert.equal(tools.find(tool => tool.name === 'mine').inputSchema.properties.session.description, 'mine');
    assert.deepEqual(ok(await client.call('typed')).result.structuredContent, { ok: true });
    assert.equal((await client.call('bad-output')).result.isError, true);
    const untyped = ok(await client.call('untyped'));
    assert.equal(Object.hasOwn(untyped.result, 'structuredContent'), false);
  } finally { await client.close(); }
});

for (const kind of ['inputSchema', 'outputSchema']) {
  test(`MCP reports invalid ${kind} without preventing unrelated tools from working`, async () => {
    const { client } = await fixture({ 'tools.js': `
defineTool({ name: 'invalid_schema', exposeAsTool: true, ${kind}: { type: 'not-a-real-type' }, handler: () => ({}) });
defineTool({ name: 'valid_schema', exposeAsTool: true, handler: () => 'ok' });
` }, ['./tools.js']);
    try {
      const result = await client.call('invalid_schema');
      assert.equal(isError(result), true);
      assert.ok(JSON.stringify(result).includes(`invalid ${kind}`));
      ok(await client.call('valid_schema'));
    } finally { await client.close(); }
  });
}
