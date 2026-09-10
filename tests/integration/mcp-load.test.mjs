import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, isError, ok, payload } from './mcp-client.mjs';

const small = dataUrl('<h1 id="t">x</h1><button id="b">go</button>');

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: 32 callers share one MCP session without losing or deadlocking calls`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: small, session: 'pile:one' }));
      await Promise.all(Array.from({ length: 32 }, async (_, index) => {
        ok(await client.call('snapshot', { session: 'pile:one' }));
        ok(await client.call('evaluate', { expression: `${index}+1`, session: 'pile:one' }));
        const reply = payload(ok(await client.call('run_script', { source: 'return await page.title()', session: 'pile:one' })));
        assert.equal(reply.status, 'ok');
      }));
    } finally { await client.close(); }
  });

  test(`${backend}: MCP contexts recover after closure races with snapshot callers`, async () => {
    const client = await McpClient.launch(backend);
    try {
      for (let round = 0; round < 5; round++) {
        ok(await client.call('navigate', { url: small, session: 'race:one' }));
        const callers = Array.from({ length: 6 }, async () => {
          for (let request = 0; request < 4; request++) {
            const reply = await client.call('snapshot', { session: 'race:one' });
            if (isError(reply)) assert.match(JSON.stringify(reply).toLowerCase(), /closed|not found/);
          }
        });
        const close = client.call('page', { action: 'close_context', session: 'race:one' });
        await Promise.all([...callers, close]);
        ok(await client.call('navigate', { url: small, session: 'race:one' }));
      }
    } finally { await client.close(); }
  });

  test(`${backend}: MCP instances recover after closure races across their sessions`, async () => {
    const client = await McpClient.launch(backend);
    try {
      for (const session of ['kill:a', 'kill:b']) ok(await client.call('navigate', { url: small, session }));
      const callers = ['kill:a', 'kill:b'].map(async session => {
        for (let request = 0; request < 6; request++) {
          const reply = await client.call('evaluate', { expression: '1+1', session });
          if (isError(reply)) assert.match(JSON.stringify(reply).toLowerCase(), /closed|gone/);
        }
      });
      await Promise.all([...callers, client.call('page', { action: 'close_instance', session: 'kill:a' })]);
      for (const session of ['kill:a', 'kill:b']) ok(await client.call('navigate', { url: small, session }));
    } finally { await client.close(); }
  });

  test(`${backend}: MCP remains responsive after large DOMs and console storms`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const big = dataUrl(`<body><script>for(let i=0;i<4000;i++){const d=document.createElement('div');d.setAttribute('role','listitem');d.textContent='row '+i;document.body.appendChild(d);}</script></body>`);
      ok(await client.call('navigate', { url: big, session: 'big:one' }));
      for (let index = 0; index < 3; index++) ok(await client.call('snapshot', { session: 'big:one' }));
      const count = payload(ok(await client.call('run_script', { source: "return await page.locator('[role=listitem]').count()", session: 'big:one' })));
      assert.equal(count.value, 4000);
      const storm = dataUrl(`<body><script>for(let i=0;i<3000;i++)console.log('storm line '+i+' '+'x'.repeat(200));</script><h1>storm</h1></body>`);
      ok(await client.call('navigate', { url: storm, session: 'storm:one' }));
      ok(await client.call('evaluate', { expression: 'document.title', session: 'storm:one' }));
      ok(await client.call('snapshot', { session: 'storm:one' }));
    } finally { await client.close(); }
  });

  test(`${backend}: MCP errors retain structured payloads and leave the session usable`, async () => {
    const client = await McpClient.launch(backend);
    try {
      for (const source of ["throw new Error('boom');", "await page.click('#nope', { timeout: 300 });"]) {
        const response = await client.call('run_script', { source, session: 'err:one' });
        assert.equal(response.result.isError, true);
        assert.equal(payload(response).status, 'error');
        ok(await client.call('evaluate', { expression: '1+1', session: 'err:one' }));
        ok(await client.call('snapshot', { session: 'err:one' }));
      }
      for (const args of [{ action: 'bogus' }, { action: 'select', page_index: 99 }]) {
        assert.equal(isError(await client.call('page', { ...args, session: 'err:one' })), true);
      }
      ok(await client.call('evaluate', { expression: '2+2', session: 'err:one' }));
    } finally { await client.close(); }
  });
}
