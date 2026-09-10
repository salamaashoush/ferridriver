import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { repo, workspace } from './support.mjs';
import { McpClient, ok } from './mcp-client.mjs';

const source = path => readFile(join(repo, path), 'utf8');
const uncomment = text => text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
function body(text, declaration) {
  text = uncomment(text);
  const start = text.indexOf(declaration);
  assert.ok(start >= 0, `missing ${declaration}`);
  const open = text.indexOf('{', start);
  assert.ok(open >= 0);
  let depth = 0;
  for (let index = open; index < text.length; index++) {
    if (text[index] === '{') depth++;
    if (text[index] === '}' && --depth === 0) return text.slice(open + 1, index);
  }
  assert.fail(`unterminated ${declaration}`);
}
function declares(text, name, fields) {
  assert.ok(fields.length > 0, `${name}: empty source contract`);
  for (const field of fields) assert.match(text, new RegExp(`\\b${field}\\??\\s*:`), `${name} omits ${field}`);
}

for (const [path, rust, types] of [
  ['crates/ferridriver-mcp/src/extension/manifest.rs', 'ToolManifest', 'ToolDefinition<'],
  ['crates/ferridriver-mcp/src/extension/manifest.rs', 'ToolAllow', 'ToolAllow'],
  ...['ExtensionManifest', 'ExtensionProvides', 'ExtensionRequires', 'ExtensionEntry'].map(name => [
    'crates/ferridriver-config/src/extension_manifest.rs', name, name === 'ExtensionManifest' ? 'ExtensionPackageManifest' : name,
  ]),
  ['crates/ferridriver-config/src/command_spec.rs', 'CommandSpec', 'CommandSpec'],
]) {
  test(`extension declarations cover every ${rust} field from the implementation`, async () => {
    const implementation = body(await source(path), `pub struct ${rust}`);
    assert.doesNotMatch(implementation, /#\[serde\([^\]]*\b(rename|flatten)\s*=/, 'update field-name extraction for explicit serde renames');
    const fields = [...implementation.matchAll(/\bpub\s+(\w+)\s*:/g)].map(match => match[1].replace(/_([a-z])/g, (_, letter) => letter.toUpperCase()));
    const declarations = await source('packages/ferridriver-extension/index.d.ts');
    const type = types === 'CommandSpec' ? body(declarations, 'export type CommandSpec') : body(declarations, `interface ${types}`);
    declares(type, types, fields);
    if (rust === 'ToolManifest') declares(type, types, ['handler']);
  });
}

test('extension host declarations cover every runtime host and contain no any escapes', async () => {
  const runtime = body(await source('crates/ferridriver-script/src/engine.rs'), 'pub fn as_str(self)');
  const hosts = [...runtime.matchAll(/Self::\w+\s*=>\s*"([^"]+)"/g)].map(match => match[1]);
  assert.ok(hosts.length > 0);
  const declarations = await source('packages/ferridriver-extension/index.d.ts');
  const union = declarations.match(/type ExtensionHost\s*=([^;]+);/)?.[1];
  assert.ok(union);
  for (const host of hosts) assert.ok(union.includes(`'${host}'`), host);
  for (const [index, line] of declarations.split('\n').entries()) {
    const code = line.split('//')[0];
    assert.doesNotMatch(code, /: any|<any/, `line ${index + 1}`);
  }
});

test('extension context declarations cover every installed key and forwarded capability', async () => {
  const root = await workspace({ 'context.js': "defineTool({ name: 'context_keys', exposeAsTool: true, handler: context => Object.keys(context) });" });
  const config = join(root, 'ferridriver.toml');
  await writeFile(config, `extensions = [${JSON.stringify(join(root, 'context.js'))}]\n`);
  const client = await McpClient.launch('cdp-pipe', config);
  try {
    const response = ok(await client.call('context_keys'));
    const result = JSON.parse(response.result.content.at(-1).text);
    const keys = result.value ?? result;
    const declaration = body(await source('packages/ferridriver-extension/index.d.ts'), 'interface ToolContext<');
    declares(declaration, 'ToolContext', keys);
    const implementation = await source('crates/ferridriver-script/src/bindings/extensions.rs');
    const constant = name => {
      const value = implementation.match(new RegExp(`pub const ${name}[^=]*= &\\[([\\s\\S]*?)\\];`))?.[1];
      assert.ok(value, name);
      return [...value.matchAll(/"([^"]+)"/g)].map(match => match[1]);
    };
    const declared = constant('TOOL_CONTEXT_KEYS');
    declares(declaration, 'ToolContext', declared);
    assert.deepEqual([...keys].sort(), [...declared].sort());
    for (const key of constant('FORWARDED_CONTEXT_KEYS')) assert.ok(declared.includes(key), key);
  } finally { await client.close(); }
});
