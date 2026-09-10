import assert from 'node:assert/strict';
import { readdir, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, dataUrl, ok, payload } from './mcp-client.mjs';

const secret = 's3cr3t-value-9f2a';
const redacted = '<secret>APP_PASSWORD</secret>';
const text = response => response.result.content.filter(block => block.type === 'text').map(block => block.text).join('\n');
async function files(root) {
  const result = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isDirectory()) result.push(...await files(path));
    else result.push(path);
  }
  return result.sort();
}
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP reports live page state, redacts secrets and retains the latest artifact`, async () => {
    const root = await workspace({ '.env.secrets': `APP_PASSWORD=${secret}\n` });
    const config = join(root, 'ferridriver.toml');
    const artifacts = join(root, 'artifacts');
    await writeFile(config, `artifactsRoot = ${JSON.stringify(artifacts)}\nartifactsMaxBytes = 1\n[secrets]\nfile = ${JSON.stringify(join(root, '.env.secrets'))}\n`);
    const client = await McpClient.launch(backend, config);
    try {
      const response = ok(await client.call('run_script', {
        source: "await page.goto(args[0]); await page.locator('#pw').fill(args[1]); console.log('the password is ' + args[1]); return 'signed in with ' + args[1];",
        args: [dataUrl('<title>Contract</title><input id=pw>'), secret], code_language: 'ts',
      }));
      const all = text(response);
      assert.match(all, /### Page/);
      assert.match(all, /- Page URL: data:text\/html/);
      assert.match(all, /- Page Title: Contract/);
      const result = payload(response);
      assert.equal(result.page.title, 'Contract');
      assert.match(result.page.url, /^data:/);
      assert.equal(all.includes(secret), false);
      assert.ok(all.includes(redacted));
      assert.equal(result.value, `signed in with ${redacted}`);
      assert.ok(result.console.some(entry => entry.message === `the password is ${redacted}`));
      assert.match(all, /### Ran ferridriver code/);
      assert.equal(result.code.find(line => line.includes('.fill(')), "await page.locator('#pw').fill(process.env['APP_PASSWORD']);");
      const evaluated = text(ok(await client.call('evaluate', { expression: "document.getElementById('pw').value" })));
      assert.equal(evaluated.includes(secret), false);
      assert.ok(evaluated.includes(redacted));
      assert.equal(text(ok(await client.call('search_page', { pattern: secret }))).includes(secret), false);
      ok(await client.call('screenshot'));
      const first = await files(artifacts);
      assert.equal(first.length, 1);
      ok(await client.call('screenshot'));
      const second = await files(artifacts);
      assert.equal(second.length, 1);
      assert.notEqual(second[0], first[0]);
      assert.equal((await stat(second[0])).isFile(), true);
    } finally { await client.close(); }
  });
}
