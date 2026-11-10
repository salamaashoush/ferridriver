import assert from 'node:assert/strict';
import { join } from 'node:path';
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

// A supervisor killed outright never signals what it started; its end of the
// pipe closes, which is all --exit-with-stdin waits for.
async function heldInput() {
  const fifo = join(await workspace({}), 'stdin');
  const made = await commands.exec('probe', { command: `mkfifo ${quote(fifo)}` });
  assert.equal(made.exitCode, 0, made.stderr);
  return fifo;
}

function running(pid) {
  return commands.exec('probe', { command: `kill -0 ${pid}` }).then((result) => result.exitCode === 0);
}

// Either may have gone already, which is what these tests wait for.
async function stopEach(...names) {
  for (const name of names) await commands.stop(name).catch(() => {});
}

async function until(condition, what, ms = 30000) {
  const deadline = Date.now() + ms;
  while (!(await condition())) {
    assert.ok(Date.now() < deadline, what);
    await new Promise((done) => setTimeout(done, 200));
  }
}

test('browser serve --exit-with-stdin closes its browser when the supervisor goes', async () => {
  test.setTimeout(120000);
  const fifo = await heldInput();
  await commands.start('ui', { command: `exec ${quote(binary)} browser serve --exit-with-stdin < ${quote(fifo)}` });
  await commands.start('stdio', { command: `exec sleep 600 > ${quote(fifo)}` });
  try {
    const endpoint = (await commands.waitForOutput('ui', '\n', 60000)).trim();
    const response = await script(`
      const browser = await chromium().connectOverCDP(${JSON.stringify(endpoint)});
      const cdp = await browser.newBrowserCDPSession();
      const info = await cdp.send('SystemInfo.getProcessInfo');
      return info.processInfo.find((process) => process.type === 'browser').id;
    `);
    const pid = response.value;
    assert.ok(await running(pid), 'the browser runs while the supervisor does');

    await commands.stop('stdio');
    await until(async () => !commands.status('ui').running, 'the server stops once its input ends');
    await until(async () => !(await running(pid)), 'the browser ends with the server');
    assert.match(commands.status('ui').stderr, /standard input closed; closing the browser/);
  } finally {
    await stopEach('stdio', 'ui');
  }
});

test('MCP over HTTP with --exit-with-stdin stops when the supervisor goes', async () => {
  test.setTimeout(60000);
  const cwd = await workspace({});
  const fifo = await heldInput();
  const command = [binary, 'mcp', '--no-inherit', '--transport', 'http', '--port', '0', '--headless', '--exit-with-stdin']
    .map(quote).join(' ');
  await commands.start('ui', { command: `cd ${quote(cwd)} && exec ${command} < ${quote(fifo)} 2>&1` });
  await commands.start('stdio', { command: `exec sleep 600 > ${quote(fifo)}` });
  try {
    await commands.waitForOutput('ui', '/mcp\n', 30000);
    assert.equal(commands.status('ui').running, true);
    await commands.stop('stdio');
    await until(async () => !commands.status('ui').running, 'the MCP server stops once its input ends');
  } finally {
    await stopEach('stdio', 'ui');
  }
});

// A supervisor reads its servers' stderr as well, so when it dies the stream
// a server reports its shutdown on is closed too. Writing there must not end
// the server before it has closed its browser.
async function heldInputAndError() {
  const input = await heldInput();
  const error = join(await workspace({}), 'stderr');
  const made = await commands.exec('probe', { command: `mkfifo ${quote(error)}` });
  assert.equal(made.exitCode, 0, made.stderr);
  return { input, error };
}

test('browser serve --exit-with-stdin closes its browser when the supervisor reading its stderr goes', async () => {
  test.setTimeout(120000);
  const { input, error } = await heldInputAndError();
  await commands.start('ui', {
    command: `exec ${quote(binary)} browser serve --exit-with-stdin < ${quote(input)} 2> ${quote(error)}`,
  });
  await commands.start('stdio', { command: `exec sleep 600 > ${quote(input)} 3< ${quote(error)}` });
  try {
    const endpoint = (await commands.waitForOutput('ui', '\n', 60000)).trim();
    const response = await script(`
      const browser = await chromium().connectOverCDP(${JSON.stringify(endpoint)});
      const cdp = await browser.newBrowserCDPSession();
      const info = await cdp.send('SystemInfo.getProcessInfo');
      return info.processInfo.find((process) => process.type === 'browser').id;
    `);
    const pid = response.value;
    assert.ok(await running(pid), 'the browser runs while the supervisor does');

    await commands.stop('stdio');
    await until(async () => !commands.status('ui').running, 'the server stops once its input ends');
    assert.equal(commands.status('ui').exitCode, 0, 'the server stops cleanly, not on a failed write');
    await until(async () => !(await running(pid)), 'the browser ends with the server');
  } finally {
    await stopEach('stdio', 'ui');
  }
});

/** One JSON-RPC message to an MCP endpoint over streamable HTTP: the answer to it, and the session id it names. */
async function mcpCall(url, token, session, message) {
  const headers = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    authorization: `Bearer ${token}`,
  };
  if (session) headers['mcp-session-id'] = session;
  const response = await fetch(url, { method: 'POST', headers, body: JSON.stringify(message) });
  const text = await response.text();
  assert.ok(response.ok, `${response.status} ${text}`);
  const answer = text.startsWith('{')
    ? JSON.parse(text)
    : text.split('\n').filter((line) => line.startsWith('data:')).map((line) => line.slice(5).trim())
      .filter((data) => data.startsWith('{')).map((data) => JSON.parse(data)).find((value) => value.id === message.id);
  return { answer, session: response.headers.get('mcp-session-id') ?? session };
}

// A remote browser session hands an agent a browser already on a page; the
// MCP server connected to it starts there, not on a blank page of its own.
test('MCP connected to a running browser starts on the page it shows', async () => {
  test.setTimeout(120000);
  await commands.start('ui', { command: `exec ${quote(binary)} browser serve` });
  try {
    const endpoint = (await commands.waitForOutput('ui', '\n', 60000)).trim();
    await script(`
      const browser = await chromium().connectOverCDP(${JSON.stringify(endpoint)});
      const cdp = await browser.newBrowserCDPSession();
      for (const target of (await cdp.send('Target.getTargets')).targetInfos) {
        if (target.type === 'page') await cdp.send('Target.closeTarget', { targetId: target.targetId });
      }
      await cdp.send('Target.createTarget', { url: 'data:text/html,<title>opened by the session</title>' });
    `);
    const cwd = await workspace({});
    const command = [binary, 'mcp', '--no-inherit', '--transport', 'http', '--port', '0', '--headless',
      '--token', 'sashoush-connect', '--connect', endpoint].map(quote).join(' ');
    await commands.start('stdio', { command: `cd ${quote(cwd)} && exec ${command} 2>&1` });
    const output = await commands.waitForOutput('stdio', '/mcp\n', 30000);
    const url = output.match(/listening on (http:\/\/127\.0\.0\.1:\d+\/mcp)/)?.[1];
    assert.ok(url, output);
    const token = 'sashoush-connect';
    const { session } = await mcpCall(url, token, undefined, {
      jsonrpc: '2.0', id: 1, method: 'initialize',
      params: { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'integration', version: '1' } },
    });
    await mcpCall(url, token, session, { jsonrpc: '2.0', method: 'notifications/initialized' });
    const { answer } = await mcpCall(url, token, session, {
      jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'snapshot', arguments: {} },
    });
    const text = answer.result.content[0].text;
    assert.match(text, /Title: opened by the session/, text);
    const { answer: pages } = await mcpCall(url, token, session, {
      jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'page', arguments: { action: 'list' } },
    });
    assert.doesNotMatch(pages.result.content[0].text, /about:blank/, pages.result.content[0].text);
  } finally {
    await stopEach('stdio', 'ui');
  }
});
