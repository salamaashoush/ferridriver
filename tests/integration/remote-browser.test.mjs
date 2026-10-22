import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { binary, fixtureServer, passed, quote, run, script, workspace } from './support.mjs';

const SPEC = `import { test, expect } from '@ferridriver/test';

test('runs on the served browser', async ({ page }) => {
  await page.setContent('<title>served</title><p id="p">hello</p>');
  expect(await page.title()).toBe('served');
  await expect(page.locator('#p')).toHaveText('hello');
});
`;

const FEATURE = `Feature: served browser
  Scenario: a page opens on it
    Given I navigate to "data:text/html,<title>served</title>"
    Then the title reads "served"
`;

const STEPS = `import { Then } from '@cucumber/cucumber';

Then('the title reads {string}', async ({ page }, expected) => {
  const actual = await page.title();
  if (actual !== expected) throw new Error(actual);
});
`;

test('test and bdd --connect run on a served browser, and only with its token', async () => {
  test.setTimeout(120000);
  const token = 'sashoush-grid-token';
  await commands.start('ui', { command: `exec ${quote(binary)} browser serve --token ${quote(token)}` });
  try {
    const endpoint = (await commands.waitForOutput('ui', '\n', 60000)).trim();
    assert.match(endpoint, /^ws:\/\/127\.0\.0\.1:\d+\/[0-9a-f]{64}\?token=sashoush-grid-token$/);
    const cwd = await workspace({ 'connect.spec.ts': SPEC, 'served.feature': FEATURE, 'steps/served.steps.ts': STEPS });

    const suite = await run(['test', '--no-inherit', '--connect', endpoint, 'connect.spec.ts'], { cwd });
    passed(suite);
    assert.match(suite.text, /1 passed/);
    const scenarios = await run(['bdd', '--no-inherit', '--connect', endpoint, '--steps', 'steps/*.ts', 'served.feature'], { cwd });
    passed(scenarios);
    assert.match(scenarios.text, /1 passed/);
    // The run closed its own contexts, not the browser it borrowed.
    assert.equal(commands.status('ui').running, true, commands.status('ui').stdout);

    const refused = await run(['test', '--no-inherit', '--connect', endpoint.replace(token, 'wrong'), 'connect.spec.ts'], { cwd });
    assert.notEqual(refused.code, 0, refused.text);
    assert.match(refused.text, /401|Unauthorized|token/i);

    const unknownPath = endpoint.replace(/\/[0-9a-f]{64}\?/, '/guess?');
    const lost = await run(['test', '--no-inherit', '--connect', unknownPath, 'connect.spec.ts'], { cwd });
    assert.notEqual(lost.code, 0, lost.text);
  } finally {
    await commands.stop('ui');
  }
});

async function mcpRequest(url, token) {
  const headers = { 'content-type': 'application/json', accept: 'application/json, text/event-stream' };
  if (token) headers.authorization = `Bearer ${token}`;
  const response = await fetch(url, {
    method: 'POST',
    headers,
    body: JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'initialize',
      params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'integration', version: '1' } },
    }),
  });
  await response.text();
  return response.status;
}

test('MCP over HTTP refuses a request without its bearer token', async () => {
  test.setTimeout(60000);
  const cwd = await workspace({});
  const command = [binary, 'mcp', '--no-inherit', '--transport', 'http', '--port', '0', '--headless',
    '--token', 'sashoush-mcp-a', '--token', 'sashoush-mcp-b'].map(quote).join(' ');
  await commands.start('ui', { command: `cd ${quote(cwd)} && exec ${command} 2>&1` });
  try {
    const output = await commands.waitForOutput('ui', '/mcp\n', 30000);
    const url = output.match(/listening on (http:\/\/127\.0\.0\.1:\d+\/mcp)/)?.[1];
    assert.ok(url, output);
    assert.equal(await mcpRequest(url), 401);
    assert.equal(await mcpRequest(url, 'sashoush-mcp-c'), 401);
    assert.equal(await mcpRequest(url, 'sashoush-mcp-a'), 200);
    assert.equal(await mcpRequest(url, 'sashoush-mcp-b'), 200);
  } finally {
    await commands.stop('ui');
  }
});

test('MCP over HTTP will not serve the network without a token', async () => {
  const result = await run(['mcp', '--no-inherit', '--transport', 'http', '--host', '0.0.0.0', '--port', '0', '--headless'],
    { cwd: await workspace({}) });
  assert.notEqual(result.code, 0, result.text);
  assert.match(result.text, /without a token/);
});

test('a script serves chromium with launchServer and another connection drives it', async () => {
  test.setTimeout(60000);
  const response = await script(`
    const server = await chromium().launchServer({ token: 'sashoush-script-token' });
    try {
      const endpoint = server.wsEndpoint();
      const refused = await chromium().connectOverCDP(endpoint.replace('sashoush-script-token', 'wrong'), { timeout: 5000 })
        .then(() => 'connected', (error) => String(error.message));
      const browser = await chromium().connectOverCDP(endpoint);
      try {
        const context = await browser.newContext();
        const page = await context.newPage();
        await page.setContent('<title>remote</title>');
        const notChromium = await firefox().launchServer().then(() => 'served', (error) => String(error.message));
        return { title: await page.title(), refused, notChromium };
      } finally {
        await browser.close();
      }
    } finally {
      await server.close();
    }
  `);
  assert.equal(response.value.title, 'remote');
  assert.notEqual(response.value.refused, 'connected');
  assert.match(response.value.notChromium, /DevTools Protocol/);
});

// The fixture server's forwarding proxies want credentials and resolve
// `*.test` to loopback, which nothing else does: a page on such a host went
// through the proxy with the credentials accepted. A client connecting over
// CDP knows neither the proxy nor its password.
test('browser serve --proxy carries every client context through the proxy, answering its credentials', async () => {
  test.setTimeout(180000);
  await fixtureServer(async (baseURL) => {
    const endpoints = await (await fetch(`${baseURL}/fx/endpoints`)).json();
    const port = new URL(baseURL).port;
    for (const [kind, given] of [['authProxy', 'flag'], ['socksProxy', 'env']]) {
      const server = new URL(endpoints[kind]);
      server.username = endpoints.proxyUser;
      server.password = endpoints.proxyPassword;
      const proxy = server.href.replace(/\/$/, '');
      const serve = given === 'flag'
        ? `exec ${quote(binary)} browser serve --proxy ${quote(proxy)}`
        : `FERRIDRIVER_BROWSER_PROXY=${quote(proxy)} exec ${quote(binary)} browser serve`;
      await commands.start('ui', { command: serve });
      try {
        const endpoint = (await commands.waitForOutput('ui', '\n', 60000)).trim();
        const key = crypto.randomUUID();
        const named = `http://${key}.proxied.test:${port}/fx/landed`;
        const loopback = `${baseURL}/fx/landed?loopback=${key}`;
        const response = await script(`
          const browser = await chromium().connectOverCDP(${JSON.stringify(endpoint)});
          const visits = [];
          for (const context of [browser.contexts()[0], await browser.newContext()]) {
            const page = await context.newPage();
            for (const url of [${JSON.stringify(named)}, ${JSON.stringify(loopback)}]) {
              const response = await page.goto(url);
              visits.push([url, response?.status(), await page.textContent('body')]);
            }
            await page.close();
          }
          return visits;
        `);
        for (const [url, status, body] of response.value) {
          assert.equal(status, 200, `${kind} ${url}`);
          assert.match(body, /landed/, `${kind} ${url}`);
        }
        const log = await (await fetch(`${baseURL}/fx/forward-log?key=${kind === 'authProxy' ? key : `127.0.0.1:${port}`}`)).json();
        const expected = kind === 'authProxy'
          ? [`http GET ${named}`, `http GET ${loopback}`]
          : [`socks 127.0.0.1:${port}`];
        // Once per context: the served browser's own and the client's.
        for (const line of expected) {
          const seen = log.lines.filter((logged) => logged === line).length;
          assert.ok(kind === 'authProxy' ? seen === 2 : seen >= 2, `${kind}: ${line} x${seen} in ${JSON.stringify(log.lines)}`);
        }
        if (kind === 'socksProxy') {
          const resolved = await (await fetch(`${baseURL}/fx/forward-log?key=${key}`)).json();
          const seen = resolved.lines.filter((logged) => logged === `socks ${key}.proxied.test:${port}`).length;
          assert.ok(seen >= 2, JSON.stringify(resolved.lines));
        }
      } finally {
        await commands.stop('ui');
      }
    }
  });
});
