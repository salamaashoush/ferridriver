import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { binary, observation, passed, quote, run, runtimeProbe, workspace } from './support.mjs';

const trace = "{\"version\":8,\"type\":\"context-options\",\"origin\":\"library\",\"browserName\":\"chromium\",\"platform\":\"darwin\",\"wallTime\":1000,\"monotonicTime\":0,\"title\":\"checkout > pays\",\"options\":{},\"sdkLanguage\":\"javascript\"}\n{\"type\":\"before\",\"callId\":\"call@1\",\"startTime\":0,\"class\":\"Page\",\"method\":\"goto\",\"title\":\"page.goto\",\"params\":{\"url\":\"http://app.local/\"},\"pageId\":\"page@1\",\"stack\":[{\"file\":\"/spec.ts\",\"line\":7,\"column\":3}]}\n{\"type\":\"after\",\"callId\":\"call@1\",\"endTime\":412}\n{\"type\":\"before\",\"callId\":\"call@2\",\"startTime\":500,\"class\":\"Locator\",\"method\":\"click\",\"title\":\"locator.click\",\"params\":{\"selector\":\"#submit\"},\"pageId\":\"page@1\",\"parentId\":\"call@1\"}\n{\"type\":\"log\",\"callId\":\"call@2\",\"time\":600,\"message\":\"waiting for locator('#submit')\"}\n{\"type\":\"after\",\"callId\":\"call@2\",\"endTime\":1700,\"error\":{\"name\":\"TimeoutError\",\"message\":\"Timeout 1000ms exceeded\"}}\n{\"type\":\"console\",\"time\":50,\"messageType\":\"error\",\"text\":\"kaboom\",\"pageId\":\"page@1\",\"location\":{\"url\":\"http://app.local/\",\"lineNumber\":3,\"columnNumber\":1}}\n";
const network = "{\"type\":\"resource-snapshot\",\"snapshot\":{\"time\":12,\"request\":{\"method\":\"GET\",\"url\":\"http://app.local/app.js\"},\"response\":{\"status\":200,\"content\":{\"mimeType\":\"text/javascript\"}}}}\n{\"type\":\"resource-snapshot\",\"snapshot\":{\"time\":3,\"request\":{\"method\":\"POST\",\"url\":\"http://app.local/api/pay\"},\"response\":{\"status\":500,\"content\":{\"mimeType\":\"application/json\"}}}}\n";
const config = '[test]\noutputDir = "test-results"\n';
const archive = 'test-results/checkout-pays/pays-attempt1.trace.zip';

async function project() {
  const { results, cwd } = await runtimeProbe([{ op: 'reporter-api', action: 'archive', path: archive,
    entries: { 'trace.trace': trace, 'trace.network': network },
  }], { 'ferridriver.toml': config });
  observation(results[0]);
  return cwd;
}

async function command(cwd, args) {
  const result = await run(['trace', ...args, '--no-inherit', '-c', join(cwd, 'ferridriver.toml')], { cwd });
  passed(result);
  return result.text;
}

test('trace show renders the newest call tree, errors, requests, and source locations', async () => {
  const out = await command(await project(), ['show']);
  for (const text of ['checkout > pays', 'page.goto http://app.local/', '412ms', 'locator.click #submit',
    'TimeoutError', "waiting for locator('#submit')", 'at /spec.ts:7', '2 requests, 1 failed', '1 message, 1 error(s)']) {
    assert.ok(out.includes(text), out);
  }
});

test('trace show errors excludes successful calls and requests', async () => {
  const out = await command(await project(), ['show', '--errors']);
  assert.ok(!out.includes('page.goto'), out);
  assert.ok(out.includes('locator.click'), out);
  assert.ok(!out.includes('app.js'), out);
  assert.ok(out.includes('/api/pay'), out);
});

test('trace show hides only the requested sections', async () => {
  const out = await command(await project(), ['show', '--hide', 'console', '--hide', 'network', '--hide', 'logs']);
  assert.ok(out.includes('locator.click'), out);
  for (const text of ['kaboom', 'app.js', 'waiting for locator']) assert.ok(!out.includes(text), out);
});

test('trace show JSON retains browser, action duration, errors, and network status', async () => {
  const context = JSON.parse(await command(await project(), ['show', '--json'])).contexts[0];
  assert.equal(context.browserName, 'chromium');
  assert.equal(context.actions[1].error.name, 'TimeoutError');
  assert.equal(context.actions[1].durationMs, 1200);
  assert.equal(context.network[1].status, 500);
});

test('trace ls lists the archive and flags its failure in text and JSON', async () => {
  const cwd = await project();
  const out = await command(cwd, ['ls']);
  assert.ok(out.includes('pays-attempt1.trace.zip'), out);
  assert.ok(out.includes('1 failed'), out);
  const entries = JSON.parse(await command(cwd, ['ls', '--json']));
  assert.equal(entries.length, 1);
  assert.ok(entries[0].summary.includes('chromium'));
});

test('missing trace errors identify the directory searched', async () => {
  const cwd = await workspace({ 'ferridriver.toml': config });
  const result = await run(['trace', 'show', '--no-inherit', '-c', join(cwd, 'ferridriver.toml')], { cwd });
  assert.notEqual(result.code, 0);
  assert.ok(result.text.includes('no traces under'), result.text);
});

test('trace view serves its embedded assets and refuses files outside the trace directory', async ({ request }) => {
  const cwd = await project();
  await commands.start('stdio', { command: `cd ${quote(cwd)} && exec ${quote(binary)} trace view --no-open --port 0 --no-inherit -c ${quote(join(cwd, 'ferridriver.toml'))}` });
  try {
    const output = await commands.waitForOutput('stdio', '\n');
    const url = output.match(/http:\/\/127\.0\.0\.1:\d+\/[^\s]+/)?.[0];
    assert.ok(url, output);
    assert.ok(url.includes('/trace/index.html?trace=file'), url);
    const base = url.split('/trace/')[0];
    for (const [path, type] of [['/trace/index.html', 'text/html'], ['/trace/sw.bundle.js', 'javascript']]) {
      const response = await request.get(base + path);
      assert.equal(response.status(), 200);
      assert.ok(response.headers()['content-type'].includes(type));
    }
    const response = await request.get(`${base}/trace/file?path=${encodeURIComponent(join(cwd, archive))}`);
    assert.equal(response.status(), 200);
    assert.deepEqual([...await response.body()].slice(0, 2), [80, 75]);
    const forbidden = await request.get(`${base}/trace/file?path=${encodeURIComponent('/etc/passwd')}`);
    assert.equal(forbidden.status(), 403);
  } finally {
    await commands.stop('stdio');
  }
});
