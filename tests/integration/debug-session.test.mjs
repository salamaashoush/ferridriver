import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { binary, passed, quote, run, workspace } from './support.mjs';

async function debugRun(files, args, body) {
  test.setTimeout(150000);
  const cwd = await workspace(files);
  const env = { FERRIDRIVER_SESSION_DIR: join(cwd, 'sessions') };
  const command = ['env', `FERRIDRIVER_SESSION_DIR=${env.FERRIDRIVER_SESSION_DIR}`, binary, ...args].map(quote).join(' ');
  await commands.start('ui', { command: `cd ${quote(cwd)} && exec ${command} 2>&1` });
  try {
    let session;
    const debug = {
      async attached() {
        const output = await commands.waitForOutput('ui', 'await testDebug.resume()');
        session = output.match(/ferridriver run --session (tw-\S+)/)?.[1];
        assert.ok(session, output);
        assert.equal(commands.status('ui').running, true);
      },
      async evaluate(source) {
        const result = await run(['run', '--no-inherit', '--session', session, '--context', 'context-0', '--json', '--eval', source], { cwd, env });
        passed(result);
        return JSON.parse(result.stdout).value;
      },
      async stopped(location) {
        await commands.waitForOutput('ui', `${location}\n`);
        const info = await debug.evaluate('return testDebug.info()');
        assert.equal(info.paused, true);
        assert.ok(info.action.location.endsWith(location), JSON.stringify(info));
        return info;
      },
      async finished(success) {
        const code = await commands.wait('ui', 120000);
        const output = commands.status('ui').stdout;
        assert.equal(code === 0, success, output);
        assert.ok(!output.includes('JS_FreeRuntime'), output);
        return output;
      },
      async listed() {
        const result = await run(['session', 'list', '--no-inherit'], { cwd, env });
        passed(result);
        return result.stdout;
      },
    };
    await body(debug);
  } finally { await commands.stop('ui'); }
}

function spec(name, body, config = '') {
  return {
    [`tests/${name}.spec.ts`]: `import { test, expect } from '@ferridriver/test';\n\ntest('${name}', async ({ page }) => {\n${body}\n});\n`,
    'ferridriver.toml': `[test]\ntestDir = "tests"\ntestMatch = ["**/*.spec.ts"]\n${config}\n[test.browser]\nheadless = true\n`,
  };
}

test('debug session steps calls and pauses at source lines while suspending the deadline', async () => {
  const files = spec('stepper', `  await page.goto('data:text/html,<title>One</title><h1 id=a>first</h1>');
  await expect(page.locator('#a')).toBeVisible();
  await page.goto('data:text/html,<title>Two</title><h1 id=b>second</h1>');
  await expect(page.locator('#b')).toBeVisible();`, 'timeout = 2000');
  await debugRun(files, ['test', '--no-inherit', '--headless', '--debug'], async debug => {
    await debug.attached();
    assert.equal((await debug.stopped('stepper.spec.ts:4')).action.title, 'page.goto');
    // Exceed the 2-second test budget while parked to prove the deadline is suspended.
    await new Promise(resolve => setTimeout(resolve, 4000));
    assert.ok(!(await debug.evaluate('return page.url()')).includes('first'));
    await debug.evaluate('await testDebug.stepOver()');
    assert.equal((await debug.stopped('stepper.spec.ts:5')).action.title, 'expect.toBeVisible');
    assert.ok((await debug.evaluate('return page.url()')).includes('first'));
    await debug.evaluate("await testDebug.pauseAt('stepper.spec.ts:7')");
    await debug.stopped('stepper.spec.ts:7');
    await debug.evaluate('await testDebug.resume()');
    await debug.finished(true);
  });
});

test('debug session BDD stops in the step body and suspends its deadline', async () => {
  await debugRun({
    'features/smoke.feature': 'Feature: debug smoke\n  Scenario: blank page\n    Given a blank page\n',
    'steps/steps.ts': "Given('a blank page', async (world: any) => {\n  await world.page.goto('data:text/html,<title>Stepped</title>');\n});\n",
    'ferridriver.toml': '[test]\nfeatures = ["features/**/*.feature"]\n[test.browser]\nheadless = true\n',
  }, ['bdd', '--no-inherit', '--headless', '--debug', '--steps', 'steps/*.ts', 'features/'], async debug => {
    await debug.attached();
    const info = await debug.stopped('steps/steps.ts:2');
    assert.equal(info.action.title, 'page.goto');
    assert.ok(info.location.endsWith('smoke.feature:2'), JSON.stringify(info));
    // Exceed the 5-second step budget while the debugger holds the step.
    await new Promise(resolve => setTimeout(resolve, 7000));
    await debug.evaluate('await testDebug.resume()');
    await debug.finished(true);
  });
});

test('debug session resumed tests still fail when their execution hangs', async () => {
  const files = spec('hangs', "  await page.goto('data:text/html,<h1>here</h1>');\n  await new Promise(() => {});", 'timeout = 2000');
  await debugRun(files, ['test', '--no-inherit', '--headless', '--debug'], async debug => {
    await debug.attached();
    await debug.stopped('hangs.spec.ts:4');
    // A hold beyond the budget must not disable the deadline after release.
    await new Promise(resolve => setTimeout(resolve, 4000));
    await debug.evaluate('await testDebug.resume()');
    assert.ok((await debug.finished(false)).includes('timed out'));
  });
});

test('debug session failure preserves the actual page and the failing verdict', async () => {
  const files = spec('leaves its page behind', "  await page.goto('data:text/html,<title>Paused</title><h1 id=marker>debug-me</h1>');\n  await expect(page.locator('#nope')).toBeVisible({ timeout: 1000 });");
  await debugRun(files, ['test', '--no-inherit', '--headless', '--debug=fail'], async debug => {
    await debug.attached();
    assert.ok(JSON.stringify(await debug.evaluate('return await page.snapshotForAI()')).includes('debug-me'));
    const info = await debug.evaluate('return testDebug.info()');
    assert.ok(info.test.includes('leaves its page behind'));
    assert.ok(info.error.includes('toBeVisible'));
    assert.equal(info.resumed, false);
    await debug.evaluate('await testDebug.resume()');
    await debug.finished(false);
  });
});

test('debug session failure mode finishes passing tests without publishing a session', async () => {
  const files = spec('passes', "  await page.goto('data:text/html,<h1 id=ok>fine</h1>');\n  await expect(page.locator('#ok')).toBeVisible();");
  await debugRun(files, ['test', '--no-inherit', '--headless', '--debug=fail'], async debug => {
    await debug.finished(true);
    assert.ok(!(await debug.listed()).split('\n').some(line => line.startsWith('tw-')));
  });
});
