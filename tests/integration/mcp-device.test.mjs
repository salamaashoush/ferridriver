// `[mcp.browser] device` and the context options beside it: the pages an
// MCP server and a session host open, and the popups those pages open,
// emulate the device; a context a script asks for does not.
import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test, devices } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';
import { McpClient, ok, payload } from './mcp-client.mjs';

// Firefox has no touch or mobile mode, so it gets a desktop device that
// still differs from the browser's own agent, size and scale.
const deviceFor = backend => (backend === 'bidi' ? 'Desktop Firefox HiDPI' : 'iPhone 15');

// What a page reports about itself once it lays out at the device width.
const observe = `
  async function seen(target) {
    await target.setContent('<meta name="viewport" content="width=device-width"><body>device</body>');
    return target.evaluate(() => ({
      agent: navigator.userAgent,
      width: window.innerWidth,
      dpr: window.devicePixelRatio,
      touch: 'ontouchstart' in window || navigator.maxTouchPoints > 0,
    }));
  }
  const opened = page.waitForEvent('popup');
  await page.evaluate(() => { window.open('about:blank'); });
  const popup = await opened;
  const result = { page: await seen(page), popup: await seen(popup) };
  await popup.close();
`;
const observePages = observe + 'return result;';
// A session script reaches the browser, and a context it makes for itself
// starts from the browser's own settings.
const observeOwnContext = observe + `
  const own = await context.browser().newContext();
  try {
    return { ...result, own: await seen(await own.newPage()) };
  } finally { await own.close(); }
`;

function assertEmulates(observed, device, agent = device.userAgent) {
  assert.equal(observed.agent, agent);
  assert.equal(observed.width, device.viewport.width);
  assert.ok(Math.abs(observed.dpr - device.deviceScaleFactor) < 0.01, JSON.stringify(observed));
  assert.equal(observed.touch, device.hasTouch);
}

function assertAll(value, device, agent) {
  assertEmulates(value.page, device, agent);
  assertEmulates(value.popup, device, agent);
  if (!value.own) return;
  assert.notEqual(value.own.agent, agent, 'a context a script made took the defaults');
  assert.ok(Math.abs(value.own.dpr - device.deviceScaleFactor) > 0.5, JSON.stringify(value.own));
}

for (const backend of ['cdp-pipe', 'cdp-raw', 'webkit', 'bidi']) {
  const name = deviceFor(backend);
  const device = devices[name];

  test(`MCP pages on ${backend} emulate the configured ${name}, an explicit agent winning`, async () => {
    const root = await workspace({ 'ferridriver.toml': `[mcp.browser]
device = ${JSON.stringify(name)}
userAgent = "sashoush-agent"
` });
    const client = await McpClient.launch(backend, join(root, 'ferridriver.toml'));
    try {
      const result = payload(ok(await client.call('run_script', { source: observePages })));
      assert.equal(result.status, 'ok', JSON.stringify(result));
      assertAll(result.value, device, 'sashoush-agent');
    } finally { await client.close(); }
  });

  test(`a session host on ${backend} opens its pages as the configured ${name}`, async () => {
    const cwd = await workspace({
      'ferridriver.json': JSON.stringify({ mcp: { browser: { device: name } } }),
    });
    const registry = await workspace({});
    const env = { FERRIDRIVER_SESSION_DIR: registry };
    const id = `device-${backend}`;
    passed(await run(['session', 'open', id, '--headless', '--backend', backend], { cwd, env }));
    try {
      const result = await run(['run', '--session', id, '--json', '--eval', observeOwnContext], { cwd, env });
      passed(result);
      const response = JSON.parse(result.stdout);
      assert.equal(response.status, 'ok', result.text);
      assert.ok(response.value.own, JSON.stringify(response.value));
      assertAll(response.value, device);
    } finally {
      passed(await run(['session', 'close', id], { cwd, env }));
    }
  });
}

test('an unknown device stops the server before it serves', async () => {
  const root = await workspace({ 'ferridriver.toml': `[mcp.browser]
device = "Nokia 3310"
` });
  const result = await run(['mcp', '--headless', '--config', join(root, 'ferridriver.toml')], { cwd: root, input: '' });
  assert.notEqual(result.code, 0, result.text);
  assert.ok(result.text.includes("unknown device 'Nokia 3310'"), result.text);
});
