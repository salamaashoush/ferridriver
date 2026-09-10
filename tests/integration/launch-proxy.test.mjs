import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, repo, runtimeProbe } from './support.mjs';

async function recordingProxy(body) {
  await commands.start('fixtures', { binary: join(repo, 'target/debug/ferridriver-fixtures') });
  try {
    const ready = await commands.waitForOutput('fixtures', '\n');
    const match = ready.match(/serving (http:\/\/127\.0\.0\.1:\d+) \(proxy (http:\/\/127\.0\.0\.1:\d+)\)/);
    assert.ok(match, ready);
    await body({ baseURL: match[1], proxy: { server: match[2], bypass: '127.0.0.1,localhost' } });
  } finally {
    await commands.stop('fixtures');
  }
}

async function assertRouted(request, baseURL) {
  const response = await request.get(`${baseURL}/fx/proxy-log`);
  const recorded = await response.json();
  assert.ok(recorded.lines.some(line => line.includes('proxy-probe.invalid')), JSON.stringify(recorded));
}

for (const [backend, browserType] of [
  ['cdp-pipe', () => chromium()],
  ['cdp-raw', () => chromium({ transport: 'ws' })],
  ['bidi', () => firefox()],
]) {
  test(`${backend}: browser launch routes HTTPS requests through its proxy`, async ({ request }) => {
    await recordingProxy(async ({ baseURL, proxy }) => {
      const browser = await browserType().launch({ headless: true, proxy });
      try {
        const page = await browser.newPage();
        try { await page.goto('https://proxy-probe.invalid/', { timeout: 5000 }); } catch {}
        await assertRouted(request, baseURL);
      } finally {
        await browser.close();
      }
    });
  });
}

test('a standalone script routes through its launch proxy without poisoning its VM', async ({ request }) => {
  await recordingProxy(async ({ baseURL, proxy }) => {
    const { results } = await runtimeProbe([
      { op: 'execute-script', source: `
        const browser = await chromium().launch({ headless: true, proxy: ${JSON.stringify(proxy)} });
        try {
          const page = await browser.newPage();
          try { await page.goto('https://proxy-probe.invalid/', { timeout: 5000 }); } catch {}
        } finally { await browser.close(); }
        return true;
      ` },
      { op: 'session-state' },
    ]);
    const outcome = observation(results[0]);
    assert.equal(outcome.status, 'ok', JSON.stringify(outcome));
    assert.equal(observation(results[1]).poisoned, false);
    await assertRouted(request, baseURL);
  });
});
