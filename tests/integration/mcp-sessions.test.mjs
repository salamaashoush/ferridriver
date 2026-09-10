import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, isError, ok, payload } from './mcp-client.mjs';

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP keeps session variables across calls and ordinary exceptions`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: dataUrl('<input id="i">') }));
      const argument = 'prompt-injection"; drop table; --';
      assert.equal(await client.script("await page.fill('#i', args[0]); return await page.inputValue('#i');", [argument]), argument);
      await client.script("vars.set('k', 'v1'); globalThis.counter = 41; globalThis.keep = 'alive'; return null;");
      assert.equal(await client.script("return vars.get('k');"), 'v1');
      assert.equal(await client.script('return ++globalThis.counter;'), 42);
      const response = await client.call('run_script', { source: "throw new Error('boom');", args: [] });
      assert.equal(isError(response), true);
      const error = payload(response);
      assert.equal(error.status, 'error');
      assert.match(error.error.message, /boom/);
      assert.equal(await client.script('return globalThis.keep;'), 'alive');
    } finally { await client.close(); }
  });

  test(`${backend}: MCP replaces a poisoned VM after a script timeout`, async () => {
    const client = await McpClient.launch(backend);
    try {
      await client.script("globalThis.keep = 'alive'; return 'ok';");
      const timed = payload(await client.call('run_script', { source: 'while (true) {}', args: [], timeout_ms: 500 }));
      assert.equal(timed.status, 'error');
      assert.equal(await client.script('return 1 + 1;'), 2);
      assert.equal(await client.script('return typeof globalThis.keep;'), 'undefined');
    } finally { await client.close(); }
  });

  test(`${backend}: MCP scripts expose timers and web globals and capture console output`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const value = await client.script(`
        const t = await new Promise(resolve => setTimeout(() => resolve('tick'), 0));
        return { t, host: new URL('https://example.com/p?x=1').host,
          b64: btoa('hi'), bytes: new TextEncoder().encode('ok').length };
      `);
      assert.deepEqual(value, { t: 'tick', host: 'example.com', b64: 'aGk=', bytes: 2 });
      const result = payload(ok(await client.call('run_script', {
        source: "console.log('hello from script'); console.warn('be careful', 42); return null;", args: [],
      })));
      assert.equal(result.status, 'ok');
      assert.ok(result.console.length >= 2);
      assert.equal(result.console[0].level, 'log');
      assert.match(result.console[0].message, /hello/);
    } finally { await client.close(); }
  });

  test(`${backend}: MCP emits requested TypeScript and Rust action code only`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: dataUrl('<button id="go">Go</button>') }));
      const source = "await page.locator('#go').click(); return 'clicked';";
      for (const [language, line] of [
        ['ts', "await page.locator('#go').click();"],
        ['rust', 'page.locator("#go").click().await?;'],
      ]) {
        const result = payload(ok(await client.call('run_script', { source, code_language: language })));
        assert.equal(result.status, 'ok');
        assert.ok(result.code.includes(line), JSON.stringify(result));
      }
      const result = payload(ok(await client.call('run_script', { source })));
      assert.equal(Object.hasOwn(result, 'code'), false);
    } finally { await client.close(); }
  });
}
