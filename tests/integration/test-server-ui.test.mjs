import assert from 'node:assert/strict';
import { readFile, readdir, stat } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { fixtureServer, workspace } from './support.mjs';
import { testServer, uiServer } from './ui-support.mjs';

async function project(extra = {}) {
  return workspace({
    'specs/pass.spec.ts': `import { test, expect } from '@ferridriver/test';
      test('renders the heading', async ({ page }) => {
        await test.step('open the page', async () => {
          await page.setContent('<h1>hello</h1>');
        }, { location: { file: 'features/hello.feature', line: 12, column: 3 } });
        await expect(page.locator('h1')).toHaveText('hello');
      });`,
    'specs/fail.spec.ts': `import { test, expect } from '@ferridriver/test';
      test('notices the wrong title', async ({ page }) => {
        await page.setContent('<h1>hello</h1>');
        expect(await page.title()).toBe('never');
      });`,
    'ferridriver.toml': `[test]
      testDir = "specs"
      testMatch = ["**/*.spec.ts"]
      workers = 1
      retries = 0
      reporter = []
      name = "cdp-pipe"
      [test.browser]
      headless = true
    `,
    ...extra,
  });
}

const args = ['test', '--ui', '--ui-port', '0', '--headless', '--no-inherit', '-c', 'ferridriver.toml'];
function entries(report) {
  return report.filter(event => event.method === 'onProject')
    .flatMap(event => event.params.project.suites).flatMap(suite => suite.entries);
}

test('test-server UI lists and runs one selected test with live steps and a trace attachment', async () => {
  await uiServer(await project(), args, async url => {
    assert.ok(url.includes('/trace/uiMode.html?ws='), url);
    await testServer(url, async ui => {
      await ui.call('initialize', { interceptStdio: true, watchTestDirs: true });
      const setup = await ui.call('runGlobalSetup');
      assert.equal(setup.status, 'passed');
      assert.equal(setup.report[0].method, 'onConfigure');
      assert.equal(setup.report[0].params.config.projects[0].name, 'cdp-pipe');
      const listed = await ui.call('listTests');
      assert.equal(listed.status, 'passed');
      const project = listed.report.find(event => event.method === 'onProject');
      assert.ok(project);
      assert.equal(project.params.project.suites.length, 2);
      assert.ok(listed.report.some(event => event.method === 'onBegin'));
      const tests = entries(listed.report);
      assert.equal(tests.length, 2);
      const passing = tests.find(entry => entry.title.includes('renders'));
      assert.ok(passing);
      const run = await ui.call('runTests', { testIds: [passing.testId], trace: 'on' });
      assert.equal(run.status, 'passed');
      const begins = ui.reports('onTestBegin');
      assert.equal(begins.length, 1);
      assert.equal(begins[0].params.testId, passing.testId);
      const resultId = begins[0].params.result.id;
      assert.equal(typeof resultId, 'string');
      const ends = ui.reports('onTestEnd');
      assert.equal(ends.length, 1);
      assert.equal(ends[0].params.result.status, 'passed');
      assert.equal(ends[0].params.result.id, resultId);
      const trace = ui.reports('onAttach').flatMap(event => event.params.attachments).find(item => item.name === 'trace');
      assert.ok(trace);
      assert.ok((await stat(trace.path)).isFile());
      const steps = ui.reports('onStepBegin');
      assert.ok(steps.length > 0);
      const located = steps.find(event => event.params.step.title === 'open the page');
      assert.ok(located);
      assert.equal(located.params.step.location.file, 'features/hello.feature');
      assert.equal(located.params.step.location.line, 12);
    });
  });
});

test('test-server UI reports the selected failing test and fails its run', async () => {
  await uiServer(await project(), args, async url => {
    await testServer(url, async ui => {
      await ui.call('initialize');
      const listed = await ui.call('listTests');
      const failing = entries(listed.report).find(entry => entry.title.includes('notices'));
      assert.ok(failing);
      const run = await ui.call('runTests', { testIds: [failing.testId], trace: 'on' });
      assert.equal(run.status, 'failed');
      const ends = ui.reports('onTestEnd');
      assert.equal(ends.length, 1);
      assert.equal(ends[0].params.result.status, 'failed');
      const message = ends[0].params.result.errors[0].message;
      assert.equal(typeof message, 'string');
      assert.ok(message.includes('never') || message.includes('expect'), message);
    });
  });
});

test('test-server live trace is readable before a blocked navigation finishes', async ({ request }) => {
  await fixtureServer(async base => {
    const key = 'live-trace';
    const cwd = await project({ 'specs/slow.spec.ts': `import { test, expect } from '@ferridriver/test';
      test('takes its time', async ({ page }) => {
        await page.setContent('<h1>slow</h1>');
        await expect(page.locator('h1')).toHaveText('slow');
        await page.goto(${JSON.stringify(base + '/fx/control/page/' + key)});
      });` });
    await uiServer(cwd, args, async url => {
      await testServer(url, async ui => {
        await ui.call('initialize');
        const running = ui.call('runTests', { grep: 'takes its time', trace: 'on' });
        try {
          assert.equal((await request.get(base + '/fx/control/held/' + key)).status(), 200);
          const output = join(cwd, 'test-results');
          let live;
          for (const dir of await readdir(output)) {
            if (!dir.startsWith('.playwright-artifacts-')) continue;
            const traces = join(output, dir, 'traces');
            for (const file of await readdir(traces)) {
              if (!file.endsWith('.trace')) continue;
              const text = await readFile(join(traces, file), 'utf8');
              if (text.length > 0) { live = text; break; }
            }
            if (live) break;
          }
          assert.ok(live, 'live trace must exist while navigation remains blocked');
          const header = JSON.parse(live.split('\n')[0]);
          assert.equal(header.type, 'context-options');
          assert.equal(header.version, 8);
        } finally { await request.get(base + '/fx/control/release/' + key); }
        assert.equal((await running).status, 'passed');
        await ui.call('ping');
      });
    });
  });
});

test('test-server lists distinct project IDs and honors project filtering', async () => {
  const cwd = await project({
    'specs/fail.spec.ts': '',
    'ferridriver.toml': `[test]
      testDir = "specs"
      testMatch = ["**/*.spec.ts"]
      workers = 1
      retries = 0
      reporter = []
      [[test.projects]]
      name = "cdp-pipe"
      [test.projects.browser]
      browser = "chromium"
      backend = "cdp-pipe"
      headless = true
      [[test.projects]]
      name = "cdp-raw"
      [test.projects.browser]
      browser = "chromium"
      backend = "cdp-raw"
      headless = true
    `,
  });
  await uiServer(cwd, args, async url => {
    let ids;
    await testServer(url, async ui => {
      await ui.call('initialize');
      const setup = await ui.call('runGlobalSetup');
      assert.deepEqual(setup.report[0].params.config.projects.map(project => project.name), ['cdp-pipe', 'cdp-raw']);
      const listed = await ui.call('listTests');
      const projects = listed.report.filter(event => event.method === 'onProject');
      assert.equal(projects.length, 2);
      ids = projects.map(event => event.params.project.suites.flatMap(suite => suite.entries)[0].testId);
      assert.notEqual(ids[0], ids[1]);
      assert.equal((await ui.call('runTests', { trace: 'on' })).status, 'passed');
      const ran = ui.reports('onTestBegin').map(event => event.params.testId);
      assert.equal(ran.length, 2);
      assert.ok(ran.includes(ids[0]) && ran.includes(ids[1]));
    });
    await testServer(url, async ui => {
      assert.equal((await ui.call('runTests', { projects: ['cdp-raw'], trace: 'on' })).status, 'passed');
      assert.deepEqual(ui.reports('onTestBegin').map(event => event.params.testId), [ids[1]]);
    });
    await testServer(url, async ui => {
      const refused = await ui.call('runTests', { reuseContext: true });
      assert.equal(refused.status, 'failed');
      assert.ok(refused.error.includes('reuseContext'));
      assert.ok(ui.reports('onError').length > 0);
    });
  });
});

test('test-server applies workers, failure limits, recording, timeout, snapshots, and reporters per run', async () => {
  const cwd = await project({
    'specs/fail2.spec.ts': `import { test, expect } from '@ferridriver/test';
      test('notices the other wrong title', async ({ page }) => {
        await page.setContent('<h1>hello</h1>'); expect(await page.title()).toBe('never either');
      });`,
    'specs/fail3.spec.ts': `import { test, expect } from '@ferridriver/test';
      test('notices a third wrong title', async ({ page }) => {
        await page.setContent('<h1>hello</h1>'); expect(await page.title()).toBe('never at all');
      });`,
    'specs/slow.spec.ts': `import { test } from '@ferridriver/test';
      test('waits around', async ({ page }) => {
        await page.setContent('<h1>slow</h1>'); await new Promise(() => {});
      });`,
    'specs/snap.spec.ts': `import { test, expect } from '@ferridriver/test';
      test('keeps its shape', async ({ page }) => {
        await page.setContent('<h1>snapshot</h1>'); await expect(page.locator('h1')).toMatchSnapshot('heading');
      });`,
    'ferridriver.toml': `[test]
      testDir = "specs"
      testMatch = ["**/*.spec.ts"]
      workers = 4
      retries = 0
      reporter = []
      name = "cdp-pipe"
      [test.browser]
      headless = true
    `,
  });
  await uiServer(cwd, args, async url => {
    await testServer(url, async ui => {
      assert.equal((await ui.call('runTests', { grep: 'notices', workers: 1, maxFailures: 1, trace: 'on' })).status, 'failed');
      assert.ok(ui.reports('onTestEnd').length < 3);
      assert.ok(ui.reports('onTestBegin').every(event => event.params.result.workerIndex === 0));
    });
    await testServer(url, async ui => {
      assert.equal((await ui.call('runTests', { grep: 'renders the heading', trace: 'off' })).status, 'passed');
      assert.deepEqual(ui.reports('onAttach').flatMap(event => event.params.attachments).filter(item => item.name === 'trace'), []);
    });
    await testServer(url, async ui => {
      assert.equal((await ui.call('runTests', { grep: 'waits around', timeout: 1000, trace: 'on' })).status, 'failed');
      const ends = ui.reports('onTestEnd');
      assert.equal(ends.length, 1);
      assert.equal(ends[0].params.result.status, 'timedOut');
      assert.equal(ends[0].params.test.timeout, 1000);
    });
    await testServer(url, async ui => {
      assert.equal((await ui.call('runTests', { grep: 'renders the heading', video: 'on', trace: 'off' })).status, 'passed');
      assert.equal(ui.reports('onAttach').flatMap(event => event.params.attachments).filter(item => item.name === 'video').length, 1);
    });
    for (const [updateSnapshots, status] of [['none', 'failed'], ['missing', 'passed']]) {
      await testServer(url, async ui => {
        assert.equal((await ui.call('runTests', { grep: 'keeps its shape', updateSnapshots, trace: 'off' })).status, status);
      });
    }
    await testServer(url, async ui => {
      assert.equal((await ui.call('runTests', { grep: 'renders the heading', reporters: ['json'], trace: 'off' })).status, 'passed');
      assert.ok((await stat(join(cwd, 'test-results/results.json'))).isFile());
    });
  });
});

test('test-server discovery failures produce an error explaining the missing tree', async () => {
  const cwd = await project({ 'specs/broken.spec.ts': "import { test } from '@ferridriver/test';\ntest('never closes', async ({ page }) => {\n" });
  await uiServer(cwd, args, async url => {
    await testServer(url, async ui => {
      await ui.call('initialize');
      const listed = await ui.call('listTests');
      assert.equal(listed.status, 'failed');
      const error = listed.report.find(event => event.method === 'onError');
      assert.ok(error);
      assert.equal(typeof error.params.error.message, 'string');
      assert.ok(error.params.error.message.length > 0);
    });
  });
});
