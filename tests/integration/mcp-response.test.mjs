import assert from 'node:assert/strict';
import { readdir, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { fixtureServer, workspace } from './support.mjs';
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

// A click that opens a tab returns before the tab has committed its first
// document, which is when it joins its context. The report that follows
// still lists it, as Playwright MCP lists tabs; the page the caller's next
// call acts on stays the opener until the caller selects the tab.
const tabsOf = response => payload(response).tabs ?? [];
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP lists the tab a script opened and stays on the opener`, async () => {
    await fixtureServer(async (base) => {
      const client = await McpClient.launch(backend);
      try {
        const opener = `${base}/fx/inner`;
        const landed = `${base}/fx/landed`;
        const opens = {
          'window.open': 'await page.evaluate((href) => { window.open(href); }, args[0]);',
          'a target=_blank link': `await page.evaluate((href) => {
            const link = document.createElement('a');
            link.href = href;
            link.target = '_blank';
            link.textContent = 'open';
            document.body.append(link);
          }, args[0]);
          await page.click('a');`,
        };
        let opened = 0;
        for (const [how, open] of Object.entries(opens)) {
          const response = ok(await client.call('run_script', {
            source: `await page.goto(args[1]); ${open} return 1;`,
            args: [landed, opener],
          }));
          opened += 1;
          assert.equal(payload(response).page.url, opener, `${how}: ${text(response)}`);
          const tabs = tabsOf(response);
          assert.deepEqual(tabs.filter(tab => tab.current).map(tab => tab.url), [opener], `${how}: ${text(response)}`);
          assert.equal(tabs.filter(tab => tab.url === landed && !tab.current).length, opened, `${how}: ${text(response)}`);
          assert.match(text(response), /### Open tabs\n- 0: \(current\) \[/, `${how}: ${text(response)}`);
          assert.ok(text(response).includes(`](${landed})`), `${how}: ${text(response)}`);
        }
        const stays = ok(await client.call('run_script', { source: 'return page.url();' }));
        assert.equal(payload(stays).value, opener, text(stays));
        const tab = tabsOf(stays).find(entry => entry.url === landed);
        ok(await client.call('page', { action: 'select', page_index: tab.index }));
        const switched = ok(await client.call('run_script', { source: 'return page.url();' }));
        assert.equal(payload(switched).value, landed, text(switched));
        assert.equal(tabsOf(switched).find(entry => entry.current).index, tab.index, text(switched));
      } finally { await client.close(); }
    });
  });
}

// A tab that opens once the call that set it off has reported belongs to
// no call: it is listed, and the page the next call acts on stays put
// until the caller selects it.
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP leaves the focus alone for a tab that opens after the call`, async () => {
    await fixtureServer(async (base) => {
      const client = await McpClient.launch(backend);
      try {
        const opener = `${base}/fx/inner`;
        const late = ok(await client.call('run_script', {
          source: 'await page.goto(args[1]); await page.evaluate((href) => { setTimeout(() => window.open(href), 300); }, args[0]); return 1;',
          args: [`${base}/fx/landed`, opener],
        }));
        assert.equal(payload(late).page.url, opener, text(late));
        const deadline = Date.now() + 15000;
        while (!text(ok(await client.call('page', { action: 'list' }))).includes('/fx/landed')) {
          assert.ok(Date.now() < deadline, 'the late tab never showed in the page list');
          await new Promise((done) => setTimeout(done, 100));
        }
        const next = ok(await client.call('run_script', { source: 'return page.url();' }));
        assert.equal(payload(next).value, opener, text(next));
        const tab = tabsOf(next).find(entry => entry.url === `${base}/fx/landed`);
        assert.ok(tab && !tab.current, text(next));
        ok(await client.call('page', { action: 'select', page_index: tab.index }));
        const selected = ok(await client.call('run_script', { source: 'return page.url();' }));
        assert.equal(payload(selected).value, `${base}/fx/landed`, text(selected));
      } finally { await client.close(); }
    });
  });
}

// Closing the current tab moves to the tab that took its place, else the
// last one, as Playwright MCP's `_onPageClosed` does.
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP moves to the neighbour of a closed current tab`, async () => {
    await fixtureServer(async (base) => {
      const client = await McpClient.launch(backend);
      try {
        ok(await client.call('navigate', { url: `${base}/fx/inner?0` }));
        for (const index of [1, 2]) ok(await client.call('page', { action: 'new', url: `${base}/fx/inner?${index}` }));
        ok(await client.call('page', { action: 'select', page_index: 1 }));
        ok(await client.call('page', { action: 'close', page_index: 1 }));
        const neighbour = ok(await client.call('run_script', { source: 'return page.url();' }));
        assert.equal(payload(neighbour).value, `${base}/fx/inner?2`, text(neighbour));
        assert.deepEqual(tabsOf(neighbour).map(tab => [tab.url, tab.current]), [
          [`${base}/fx/inner?0`, false],
          [`${base}/fx/inner?2`, true],
        ], text(neighbour));
        ok(await client.call('page', { action: 'close', page_index: 1 }));
        const last = ok(await client.call('run_script', { source: 'return page.url();' }));
        assert.equal(payload(last).value, `${base}/fx/inner?0`, text(last));
      } finally { await client.close(); }
    });
  });
}
