import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { listZipEntries, readZipEntry } from '../e2e/helpers/unzip.ts';
import { repo } from './support.mjs';
import { uiServer, websocket } from './ui-support.mjs';

test('Rust harness UI selects a test, serves its trace, and survives cancelling the next run', async ({ request }) => {
  test.setTimeout(600000);
  await uiServer(repo, ['rust-test', '--ui', '--headless', '-p', 'rust-e2e-example'], async url => {
    const base = new URL(url).origin;
    const index = await request.get(base);
    assert.equal(index.status(), 200);
    assert.ok((await index.text()).includes('ferridriver UI'));
    await websocket(base.replace('http:', 'ws:') + '/ws', async socket => {
      let testId;
      let idle = false;
      while (!testId || !idle) {
        const event = await socket.next();
        if (event.type === 'testList') {
          for (const suite of event.suites) {
            const found = suite.tests.find(test => test.id.includes('lists_seeded_users'));
            if (found) testId = found.id;
          }
        }
        if (event.type === 'watchStatus') idle = event.status === 'idle';
      }
      await socket.send({ cmd: 'runTest', id: testId });
      let started = false;
      let liveTrace;
      let outcome;
      let totals;
      while (!totals) {
        const event = await socket.next();
        if (event.type === 'runStarted') {
          started = true;
          assert.equal(event.totalTests, 1);
        }
        if (event.type === 'testStarted' && event.id === testId) liveTrace = event.liveTraceUrl;
        if (event.type === 'testFinished' && event.id === testId) outcome = event.outcome;
        if (event.type === 'runFinished') totals = event.totals;
      }
      assert.equal(started, true);
      assert.equal(typeof liveTrace, 'string');
      assert.ok(liveTrace.startsWith('http://127.0.0.1:'));
      assert.ok(liveTrace.includes('/live-trace?key='));
      assert.equal(totals.total, 1);
      assert.equal(totals.passed, 1);
      assert.equal(totals.failed, 0);
      assert.ok(outcome);
      assert.equal(outcome.status, 'passed');
      const trace = outcome.attachments.find(attachment => attachment.name === 'trace');
      assert.ok(trace);
      assert.ok(trace.urlPath.startsWith('/artifact/'));
      const response = await request.get(base + trace.urlPath);
      assert.equal(response.status(), 200);
      const bytes = new Uint8Array(await response.body());
      const entry = listZipEntries(bytes).find(entry => entry.name === 'trace.trace');
      assert.ok(entry);
      const text = new TextDecoder().decode(readZipEntry(bytes, entry));
      const first = JSON.parse(text.split('\n')[0]);
      assert.equal(first.type, 'context-options');
      assert.equal(first.version, 8);
      assert.ok(text.split('\n').some(line => line.includes('"type":"before"')));

      await socket.send({ cmd: 'runAll' });
      for (;;) { if ((await socket.next()).type === 'testStarted') break; }
      await socket.send({ cmd: 'stop' });
      let cancelled = false;
      for (;;) {
        const event = await socket.next();
        if (event.type === 'runCancelled') cancelled = true;
        assert.notEqual(event.type, 'runFinished', 'Stop must not emit runFinished');
        if (event.type === 'watchStatus' && event.status === 'idle') break;
      }
      assert.equal(cancelled, true);
      assert.equal((await request.get(base)).status(), 200);
    });
  });
});
