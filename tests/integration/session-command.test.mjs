import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

async function openSession(id, url = 'data:text/html,<p>seed</p>', files = {}, flags = []) {
  const cwd = await workspace(files);
  const registry = await workspace({});
  const command = args => run(args, { cwd, env: { FERRIDRIVER_SESSION_DIR: registry, FERRIDRIVER_NO_INHERIT: '1' } });
  const opened = await command(['session', 'open', id, '--headless', ...flags, url]);
  passed(opened);
  let closed = false;
  return { cwd, command, opened,
    script: (source, flags = []) => command(['run', '--session', id, ...flags, '--eval', source]),
    async close() {
      if (closed) return;
      const result = await command(['session', 'close', id]);
      passed(result);
      closed = true;
      return result;
    },
  };
}

test('named sessions preserve the live page, console and VM state until closed', async () => {
  const session = await openSession('itest', 'data:text/html,<h1>cli-itest</h1><button>go</button>');
  try {
    assert.ok(session.opened.stdout.includes('session itest open'));
    const listed = await session.command(['session', 'list']);
    passed(listed);
    assert.ok(listed.stdout.includes('itest'));
    const attached = await session.command(['session', 'attach', 'itest']);
    passed(attached);
    assert.ok(attached.stdout.includes('cli-itest'));
    const url = await session.script('return page.url()');
    passed(url);
    assert.ok(url.stdout.includes('data:text/html'));
    const evaluated = await session.script("return await page.evaluate('1 + 2')");
    passed(evaluated);
    assert.ok(evaluated.stdout.includes('3'));
    const console = await session.script("console.log('from-the-host'); return 'done'");
    passed(console);
    assert.ok(console.stdout.includes('from-the-host'));
    passed(await session.script('globalThis.carried = 41; return null'));
    const carried = await session.script('return globalThis.carried + 1');
    passed(carried);
    assert.ok(carried.stdout.includes('42'));
    const thrown = await session.script("throw new Error('boom')");
    assert.notEqual(thrown.code, 0);
    assert.ok(thrown.stderr.includes('boom'));
    passed(await session.script("return 'still alive'"));
    const result = await session.script("console.log('in-json'); return 7", ['--json']);
    passed(result);
    const doc = JSON.parse(result.stdout);
    assert.equal(doc.status, 'ok');
    assert.equal(doc.value, 7);
    assert.ok(doc.console.some(entry => entry.message === 'in-json'));
    assert.ok((await session.close()).stdout.includes('closed'));
    const empty = await session.command(['session', 'list']);
    passed(empty);
    assert.ok(empty.stdout.includes('no live sessions'));
  } finally { await session.close(); }
});

test('session action traces stream only when requested', async () => {
  const session = await openSession('traced', 'data:text/html,<button>go</button>');
  try {
    const source = "await page.goto('data:text/html,<button>hi</button>'); await page.locator('button').click(); return 'clicked';";
    const traced = await session.script(source, ['--trace']);
    passed(traced);
    assert.ok(traced.stdout.includes('clicked'));
    for (const line of ['\u203a page.goto', '\u203a locator.click', "waiting for locator('button')", '\u2713 locator.click']) assert.ok(traced.stderr.includes(line), traced.text);
    const plain = await session.script(source);
    passed(plain);
    assert.ok(!plain.stderr.includes('page.goto'));
  } finally { await session.close(); }
});

test('session code recording produces a TypeScript file that replays against the same page', async () => {
  const session = await openSession('recorder');
  try {
    const source = "await page.goto('data:text/html,<button>go</button>'); await page.locator('button').click(); return 'done';";
    const echoed = await session.script(source, ['--code']);
    passed(echoed);
    assert.ok(echoed.stderr.includes("await page.locator('button').click();"));
    const generated = join(session.cwd, 'from-session.ts');
    passed(await session.script(source, ['--code-out', generated]));
    assert.ok((await readFile(generated, 'utf8')).includes("await page.locator('button').click();"));
    passed(await session.command(['run', '--session', 'recorder', generated]));
  } finally { await session.close(); }
});

test('session reports describe the resulting page and redact secrets before sending them to clients', async () => {
  const secret = 's3cr3t-cli-71b4';
  const session = await openSession('reported', 'data:text/html,<p>seed</p>', {
    '.env.secrets': `APP_PASSWORD=${secret}\n`, 'ferridriver.toml': '[secrets]\nfile = "./.env.secrets"\n',
  }, ['--config', 'ferridriver.toml']);
  try {
    const source = "await page.goto('data:text/html,<title>Reported</title><input id=pw>'); await page.locator('#pw').fill(args[0]); return 'signed in with ' + args[0];";
    const result = await session.command(['run', '--session', 'reported', '--report', '--code', '--eval', source, '--', secret]);
    passed(result);
    for (const text of ['### Result', '### Ran ferridriver code', '### Page', '- Page Title: Reported', '<secret>APP_PASSWORD</secret>', "await page.locator('#pw').fill(process.env['APP_PASSWORD']);"]) assert.ok(result.stdout.includes(text), result.text);
    assert.ok(!result.text.includes(secret));
    const json = await session.command(['run', '--session', 'reported', '--report', '--json', '--eval', source, '--', secret]);
    passed(json);
    assert.ok(JSON.parse(json.stdout).report.page.includes('- Page Title: Reported'));
    assert.ok(!json.stdout.includes(secret));
    const plain = await session.script(source.replaceAll('args[0]', "'plain'"));
    passed(plain);
    assert.ok(!plain.stdout.includes('### Page'));
  } finally { await session.close(); }
});

test('clients bundle local TypeScript imports before executing them in a detached session', async () => {
  const session = await openSession('modules', 'data:text/html,<p>mod</p>');
  try {
    await writeFile(join(session.cwd, 'helper.ts'), "export const title = (): string => 'from-module';");
    await writeFile(join(session.cwd, 'entry.ts'), "import { title } from './helper'; export default `${title()}:${page.url().slice(0, 4)}`;");
    const result = await session.command(['run', '--session', 'modules', 'entry.ts']);
    passed(result);
    assert.ok(result.stdout.includes('from-module:data'));
  } finally { await session.close(); }
});

test('opening an existing session ID fails without replacing its owner', async () => {
  const session = await openSession('dup');
  try {
    const result = await session.command(['session', 'open', 'dup', '--headless']);
    assert.notEqual(result.code, 0);
    assert.ok(result.stderr.includes('already exists'));
  } finally { await session.close(); }
});

for (const [title, args, message] of [
  ['missing sessions fail with their name', ['run', '--session', 'ghost', '--eval', 'return 1'], 'ghost'],
  ['extensions must be installed when the session host opens', ['run', '--session', 'any', '--extension', './x.ts', '--eval', 'return 1'], 'session open'],
]) {
  test(title, async () => {
    const cwd = await workspace({});
    const result = await run(args, { cwd, env: { FERRIDRIVER_SESSION_DIR: cwd, FERRIDRIVER_NO_INHERIT: '1' } });
    assert.notEqual(result.code, 0);
    assert.ok(result.stderr.includes(message), result.text);
  });
}
