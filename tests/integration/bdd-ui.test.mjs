import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { test } from '@ferridriver/test';
import { listZipEntries, readZipEntry } from '../e2e/helpers/unzip.ts';
import { fixtureServer, workspace } from './support.mjs';
import { testServer, uiServer } from './ui-support.mjs';

function validateTrace(bytes) {
  const entries = listZipEntries(bytes);
  const text = name => {
    const entry = entries.find(entry => entry.name === name);
    assert.ok(entry, `missing ${name}`);
    return new TextDecoder().decode(readZipEntry(bytes, entry));
  };
  const lines = text('trace.trace').trimEnd().split('\n').map(JSON.parse);
  assert.ok(entries.some(entry => entry.name === 'trace.network'));
  assert.equal(lines[0].type, 'context-options');
  assert.equal(lines[0].version, 8);
  const before = lines.filter(line => line.type === 'before');
  const step = before.find(action => action.title === 'Given a blank ui page');
  assert.ok(step);
  assert.equal(typeof step.callId, 'string');
  assert.equal(typeof step.stepId, 'string');
  assert.ok(step.stepId.length > 0);
  const stepAfter = lines.find(line => line.type === 'after' && line.callId === step.callId);
  assert.ok(stepAfter);
  assert.ok(stepAfter.endTime >= step.startTime);
  const goto = before.find(action => action.method === 'goto');
  assert.ok(goto);
  assert.equal(goto.parentId, step.callId);
  assert.equal(typeof goto.callId, 'string');
  const snapshots = lines.filter(line => line.type === 'frame-snapshot');
  assert.ok(snapshots.length > 0);
  const gotoAfter = lines.find(line => line.type === 'after' && line.callId === goto.callId);
  assert.ok(gotoAfter);
  for (const [event, kind] of [[goto, 'beforeSnapshot'], [gotoAfter, 'afterSnapshot']]) {
    assert.equal(typeof event[kind], 'string');
    assert.ok(snapshots.some(snapshot => snapshot.snapshot.snapshotName === event[kind]));
  }
  assert.ok(goto.stack[0].file.endsWith('steps.js'));
  const source = step.stack[0];
  assert.ok(source.file.endsWith('smoke.feature'));
  assert.equal(source.line, 3);
  const hash = createHash('sha1').update(source.file).digest('hex');
  assert.ok(text(`resources/src@${hash}.txt`).includes('Given a blank ui page'));
}

test('BDD UI lists scenarios, serves complete traces, and remains reusable after stopping a run', async ({ request }) => {
  await fixtureServer(async base => {
    const cwd = await workspace({
      'features/smoke.feature': 'Feature: UI smoke\n  Scenario: blank page\n    Given a blank ui page\n',
      'features/slow.feature': 'Feature: UI slow\n  Scenario: slow page\n    Given a slow ui step\n',
      'steps/steps.js': `Given("a blank ui page", async (world) => { await world.page.goto("about:blank"); });
        Given("a slow ui step", async (world) => { await world.page.goto(${JSON.stringify(base + '/fx/control/page/bdd-stop')}); });`,
    });
    await uiServer(cwd, ['bdd', '--ui', '--ui-port', '0', '--headless', '--no-inherit',
      '--steps', 'steps/*.js', 'features/**/*.feature'], async url => {
      assert.ok(url.includes('/trace/uiMode.html?ws='), url);
      const origin = new URL(url).origin;
      const index = await request.get(origin + '/trace/uiMode.html');
      assert.equal(index.status(), 200);
      assert.ok((await index.text()).includes('Playwright'));
      const worker = await request.get(origin + '/trace/sw.bundle.js');
      assert.ok(worker.headers()['content-type'].toLowerCase().includes('javascript'));
      await testServer(url, async ui => {
        await ui.call('initialize', { watchTestDirs: true });
        const listed = await ui.call('listTests');
        const project = listed.report.find(event => event.method === 'onProject');
        assert.ok(project);
        const scenarios = project.params.project.suites.flatMap(suite => suite.entries)
          .flatMap(entry => entry.entries ?? [entry]).filter(entry => entry.testId && entry.title);
        const blank = scenarios.find(entry => entry.title.includes('blank page'));
        const slow = scenarios.find(entry => entry.title.includes('slow page'));
        assert.ok(blank);
        assert.ok(slow);
        assert.equal((await ui.call('runTests', { testIds: [blank.testId], trace: 'on' })).status, 'passed');
        assert.equal((await ui.waitReport('onTestEnd')).params.result.status, 'passed');
        const attached = await ui.waitReport('onAttach');
        const trace = attached.params.attachments.find(attachment => attachment.name === 'trace');
        assert.ok(trace);
        assert.equal(trace.contentType, 'application/zip');
        const response = await request.get(`${origin}/trace/file?path=${encodeURIComponent(trace.path)}`);
        assert.equal(response.status(), 200);
        validateTrace(new Uint8Array(await response.body()));
        assert.equal((await request.get(origin + '/trace/file?path=%2Fetc%2Fpasswd')).status(), 403);

        const runId = await ui.send('runTests', { testIds: [slow.testId], trace: 'on' });
        for (;;) {
          const event = await ui.next();
          if (event.method === 'report' && event.params.method === 'onTestBegin') break;
        }
        await request.get(base + '/fx/control/held/bdd-stop');
        await ui.call('stopTests');
        await request.get(base + '/fx/control/release/bdd-stop');
        let ended = false;
        for (;;) {
          const event = await ui.next();
          if (event.method === 'report' && event.params.method === 'onTestEnd') ended = true;
          if (event.id === runId) break;
        }
        assert.equal(ended, true, 'a stopped run reports the test it was running');
        assert.equal((await ui.call('runTests', { testIds: [blank.testId], trace: 'on' })).status, 'passed');
      });
    });
  });
});
