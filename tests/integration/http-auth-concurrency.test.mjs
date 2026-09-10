import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { fixtureServer } from './fixture-server.mjs';
import { passed, run, workspace } from './support.mjs';

test('Firefox applies credentials independently to concurrent contexts', async () => {
  const server = await fixtureServer();
  try {
    const source = `const browser = await firefox().launch({ headless: true });
      try {
        return await Promise.all(Array.from({ length: 16 }, async (_, index) => {
          const context = await browser.newContext({});
          try {
            const page = await context.newPage();
            await context.setHTTPCredentials({ username: 'user', password: 'pass' });
            const response = await page.goto(${JSON.stringify(server.url + '/fx/auth')});
            return { index, status: response?.status(), body: await page.evaluate(() => document.body.textContent) };
          } finally { await context.close(); }
        }));
      } finally { await browser.close(); }`;
    const result = await run(['run', '--no-inherit', '--json', '-e', source], {
      cwd: await workspace({}), env: { RUST_LOG: 'ferridriver::backend::bidi::page=debug' },
    });
    passed(result);
    const response = JSON.parse(result.stdout);
    assert.equal(response.status, 'ok', result.text);
    assert.equal(response.value.length, 16);
    for (const value of response.value) {
      assert.equal(value.status, 200, `${JSON.stringify(value)}\n${result.stderr}`);
      assert.ok(value.body.includes('AUTHED'), JSON.stringify(value));
    }
  } finally { await server.close(); }
});
