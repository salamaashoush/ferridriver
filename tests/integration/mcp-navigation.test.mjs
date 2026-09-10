import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';
import { workspace } from './support.mjs';
import { fixtureServer } from './fixture-server.mjs';

test('MCP commit, none and DOM-ready navigation return while a subresource is still loading', async () => {
  const root = await workspace({ 'ferridriver.toml': '[mcp.server]\nsettleTimeoutMs = 0\n' });
  const client = await McpClient.launch('cdp-pipe', `${root}/ferridriver.toml`);
  const server = await fixtureServer();
  try {
    for (const wait_until of ['none', 'commit', 'domcontentloaded']) {
      ok(await client.call('navigate', { url: `${server.url}/fx/control/page/${wait_until}`, wait_until }));
      const state = await client.script(`return await page.evaluate(async () => {
        await fetch('/fx/control/held/${wait_until}');
        const state = document.readyState;
        await fetch('/fx/control/release/${wait_until}');
        return state;
      });`);
      assert.notEqual(state, 'complete', wait_until);
    }
    for (const wait_until of ['load', 'networkidle']) {
      ok(await client.call('navigate', { url: dataUrl('<h1>Loaded</h1>'), wait_until }));
      assert.equal(await client.script('return await page.evaluate(() => document.readyState);'), 'complete');
    }
  } finally { await client.close(); await server.close(); }
});

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP navigation, tab listing, reload and history reflect the live page`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const navigate = ok(await client.call('navigate', { url: dataUrl('<h1>Hello</h1>') }));
      assert.match(navigate.result.content[0].text, /Hello/);
      const listed = ok(await client.call('page', { action: 'list' }));
      assert.match(listed.result.content[0].text, /Page 0/);
      ok(await client.call('navigate', { url: dataUrl('<body>original</body>') }));
      ok(await client.call('evaluate', { expression: "document.body.textContent = 'modified'" }));
      const modified = ok(await client.call('evaluate', { expression: 'document.body.textContent' }));
      assert.match(modified.result.content[0].text, /modified/);
      ok(await client.call('page', { action: 'reload' }));
      const restored = ok(await client.call('evaluate', { expression: 'document.body.textContent' }));
      assert.match(restored.result.content[0].text, /original/);
      for (const title of ['Page1', 'Page2']) {
        ok(await client.call('navigate', { url: dataUrl(`<h1>${title}</h1>`) }));
      }
      ok(await client.call('page', { action: 'back' }));
      const previous = ok(await client.call('evaluate', { expression: "document.querySelector('h1')?.textContent || ''" }));
      assert.match(previous.result.content[0].text, /Page1/);
    } finally { await client.close(); }
  });

  test(`${backend}: MCP script URL waits honor lifecycle states and the requested timeout`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: dataUrl('<h1>here</h1>') }));
      const result = await client.script(`
        await page.waitForURL(/^data:/, { waitUntil: 'domcontentloaded' });
        await page.waitForURL(/^data:/, { waitUntil: 'commit' });
        const start = Date.now();
        let failed = false, message = '';
        try { await page.waitForURL(/never-matches/, { timeout: 400 }); }
        catch (error) { failed = true; message = String(error.message); }
        return { failed, message, elapsed: Date.now() - start };
      `);
      assert.equal(result.failed, true);
      assert.ok(result.elapsed < 3000, JSON.stringify(result));
      assert.match(result.message, /400/);
      assert.equal(await client.script(`
        await page.waitForLoadState('load', { timeout: 1 });
        await page.waitForLoadState('networkidle', { timeout: 10000 });
        return 'ok';
      `), 'ok');
    } finally { await client.close(); }
  });

  test(`${backend}: MCP tracks pushState, replaceState and fragment navigation`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const result = await client.script(`
        await page.route('http://example.test/**', route => route.fulfill({
          status: 200, contentType: 'text/html', body: '<h1 id="home">home</h1>'
        }));
        await page.goto('http://example.test/home');
        await page.evaluate("history.pushState({}, '', '/file/1234')");
        await page.waitForURL(/file\\/1234/, { timeout: 5000, waitUntil: 'commit' });
        const pushed = page.url();
        await page.evaluate("history.replaceState({}, '', '/settings')");
        await expect(page).toHaveURL(/settings/, { timeout: 5000 });
        await page.evaluate("location.hash = 'frag'");
        await page.waitForURL(/#frag/, { timeout: 5000, waitUntil: 'commit' });
        return { pushed };
      `);
      assert.match(result.pushed, /\/file\/1234/);
    } finally { await client.close(); }
  });
}
