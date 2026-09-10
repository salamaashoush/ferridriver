import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { binary, passed, quote, run, script, workspace } from './support.mjs';
import { fixtureServer } from './fixture-server.mjs';

async function cli(args, options = {}) {
  const cwd = options.cwd ?? await workspace({});
  return run(['run', '--no-inherit', ...args], { ...options, cwd });
}

const recording = `const browser = await chromium().launch({ headless: true });
  const page = await browser.newPage();
  await page.goto('data:text/html,<button>go</button>');
  await page.locator('button').click(); return 'done';`;

test('inline run scripts launch their own browser and return its page title', async () => {
  const { value } = await script(`const browser = await chromium().launch({ headless: true });
    try { const page = await (await browser.newContext()).newPage();
      await page.goto('data:text/html,<title>RunCmd</title>'); return await page.title();
    } finally { await browser.close(); }`);
  assert.equal(value, 'RunCmd');
});

test('run files receive positional arguments after the separator', async () => {
  const cwd = await workspace({ 's.js': 'return { argc: args.length, first: args[0], sum: 1 + 2 };' });
  const result = await cli(['--json', 's.js', '--', 'alpha', 'beta'], { cwd });
  passed(result);
  assert.deepEqual(JSON.parse(result.stdout).value, { argc: 2, first: 'alpha', sum: 3 });
});

test('run accepts a script through stdin', async () => {
  const result = await cli(['--json', '-'], { input: 'return 6 * 7;' });
  passed(result);
  assert.equal(JSON.parse(result.stdout).value, 42);
});

test('run reports script errors as a nonzero exit and a JSON error document', async () => {
  const result = await cli(['--json', '-e', "throw new Error('boom-run')"]);
  assert.notEqual(result.code, 0);
  assert.equal(JSON.parse(result.stdout).status, 'error');
  assert.match(result.stderr, /boom-run/);
});

test('streaming run routes console levels and the result to the correct streams', async () => {
  const result = await cli(['-e', `console.log('out-log'); console.info('out-info'); console.debug('out-debug');
    console.warn('err-warn'); console.error('err-error'); return 'done';`]);
  passed(result);
  const lines = text => text.split('\n').map(line => line.trim()).filter(Boolean);
  assert.deepEqual(lines(result.stdout), ['out-log', 'out-info', 'out-debug', 'done']);
  assert.deepEqual(lines(result.stderr), ['err-warn', 'err-error']);
  assert.doesNotMatch(result.stdout, /duration_ms/);
});

test('standalone scripts expose every supported console method', async () => {
  const { value } = await script(`const names = ['log','info','warn','error','debug','trace','dir','dirxml','table',
    'group','groupCollapsed','groupEnd','count','countReset','time','timeEnd','timeLog',
    'assert','clear','profile','profileEnd','timeStamp'];
    return names.filter(name => typeof console[name] !== 'function');`);
  assert.deepEqual(value, []);
});

test('console timers and inspection preserve warning text and uncolored output', async () => {
  const response = await script(`console.time('t'); console.time('t'); console.timeEnd('nope');
    console.dir({ n: 1 }); console.dirxml('x'); console.profile(); return null;`);
  assert.deepEqual(response.console.map(entry => [entry.level, entry.message]), [
    ['warn', "Label 't' already exists for console.time()"],
    ['warn', "No such label 'nope' for console.timeEnd()"],
    ['log', '{ n: 1 }'], ['log', 'x'],
  ]);
});

test('streaming run sends traces and failed console assertions to stderr', async () => {
  const result = await cli(['-e', "console.trace('tracing'); console.assert(false, 'nope'); return null;"]);
  passed(result);
  assert.equal(result.stdout.trim(), '');
  assert.match(result.stderr, /Trace: tracing/);
  assert.match(result.stderr, /Assertion failed: nope/);
});

test('streaming console output is readable before the script finishes', async () => {
  const server = await fixtureServer();
  const cwd = await workspace({});
  const source = `console.log('early'); await fetch(${JSON.stringify(server.url + '/fx/control/hold/stream')}); return 1;`;
  try {
    await commands.open('stdio', { command: `cd ${quote(cwd)} && exec ${[binary, 'run', '--no-inherit', '-e', source].map(quote).join(' ')}` });
    assert.equal(await commands.read('stdio'), 'early');
    assert.equal((await commands.status('stdio')).running, true);
    assert.equal((await fetch(server.url + '/fx/control/release/stream')).status, 200);
    assert.equal(await commands.read('stdio'), '1');
    assert.equal(await commands.wait('stdio'), 0);
  } finally {
    await commands.stop('stdio');
    await server.close();
  }
});

test('streaming failures preserve earlier stdout without printing JSON', async () => {
  const result = await cli(['-e', "console.log('before'); throw new Error('boom-stream');"]);
  assert.notEqual(result.code, 0);
  assert.equal(result.stdout.trim(), 'before');
  assert.match(result.stderr, /boom-stream/);
  assert.doesNotMatch(result.text, /"status"/);
});

test('JSON run output buffers console entries with timestamps that include awaiting', async () => {
  const result = await cli(['--json', '-e', "console.log('a'); await new Promise(resolve => setTimeout(resolve, 120)); console.error('b'); return 1;"]);
  passed(result);
  assert.equal(result.stderr.trim(), '');
  const entries = JSON.parse(result.stdout).console;
  assert.deepEqual(entries.map(entry => [entry.level, entry.message]), [['log', 'a'], ['error', 'b']]);
  for (const entry of entries) assert.ok(Number.isInteger(entry.ts_ms) && entry.ts_ms >= 0);
  assert.ok(entries[1].ts_ms >= entries[0].ts_ms + 100, JSON.stringify(entries));
});

test('standalone browser factories launch the browser named by their API', async () => {
  for (const factory of ['chromium', 'firefox']) {
    const { value } = await script(`const browser = await ${factory}().launch({ headless: true });
      try { return await browser.version(); } finally { await browser.close(); }`);
    if (factory === 'chromium') assert.match(value, /^(Chrome|Chromium|HeadlessChrome)\//);
    else assert.match(value, /firefox/i);
  }
});

test('run transpiles a TypeScript file and returns its default export', async () => {
  assert.equal((await script('const n: number = 19 + 23; export default n;', {}, { module: true })).value, 42);
});

test('run bundles a relative TypeScript import before executing it', async () => {
  const { value } = await script("import { triple } from './helper'; export default triple(14);", {
    'helper.ts': 'export const triple = (n: number): number => n * 3;',
  }, { module: true });
  assert.equal(value, 42);
});

test('run modules without a default export return null', async () => {
  const { value } = await script('export const x = 1; const _y = x + 1;', {}, { module: true });
  assert.equal(value, null);
});

test('run detects module syntax in inline source', async () => {
  assert.equal((await script('export default Math.max(1, 41) + 1;')).value, 42);
});

test('run echoes recorded actions in the requested language only when enabled', async () => {
  const typescript = await cli(['--code', '-e', recording]);
  passed(typescript);
  assert.ok(typescript.stderr.includes("await page.goto('data:text/html,<button>go</button>');"));
  assert.ok(typescript.stderr.includes("await page.locator('button').click();"));
  const rust = await cli(['--code', 'rust', '-e', recording]);
  passed(rust);
  assert.ok(rust.stderr.includes('page.locator("button").click().await?;'));
  const plain = await cli(['-e', recording]);
  passed(plain);
  assert.doesNotMatch(plain.stderr, /locator/);
});

test('a recorded standalone script replays successfully from its generated file', async () => {
  const cwd = await workspace({});
  passed(await cli(['--code-out', 'generated.ts', '-e', recording], { cwd }));
  const source = await readFile(join(cwd, 'generated.ts'), 'utf8');
  assert.ok(source.includes('await page.goto('));
  assert.ok(source.includes("await page.locator('button').click();"));
  assert.doesNotMatch(source, /__page/);
  passed(await cli(['generated.ts'], { cwd }));
});

test('run reports include result and code sections without inventing a bound page', async () => {
  const reported = await cli(['--report', '--code', '-e', recording]);
  passed(reported);
  assert.ok(reported.stdout.includes('### Result\ndone'));
  assert.ok(reported.stdout.includes('### Ran ferridriver code'));
  assert.ok(reported.stdout.includes("await page.locator('button').click();"));
  assert.ok(!reported.stdout.includes('### Page'));
  const plain = await cli(['-e', recording]);
  passed(plain);
  assert.equal(plain.stdout.trim(), 'done');
});

test('declared secrets are redacted from results console output and generated code', async () => {
  const secret = 's3cr3t-local-4c19';
  const cwd = await workspace({
    '.env.secrets': `APP_PASSWORD=${secret}\n`,
    'ferridriver.toml': '[secrets]\nfile = "./.env.secrets"\n',
  });
  const source = `const browser = await chromium().launch({ headless: true }); const page = await browser.newPage();
    await page.goto('data:text/html,<input id=pw>'); await page.locator('#pw').fill(args[0]);
    console.log('the password is ' + args[0]); return 'signed in with ' + args[0];`;
  const result = await cli(['--config', 'ferridriver.toml', '--json', '--code', '-e', source, '--', secret], { cwd });
  passed(result);
  assert.ok(!result.text.includes(secret));
  const response = JSON.parse(result.stdout);
  assert.equal(response.value, 'signed in with <secret>APP_PASSWORD</secret>');
  assert.ok(response.console.some(entry => entry.message === 'the password is <secret>APP_PASSWORD</secret>'));
  assert.ok(response.code.includes("await page.locator('#pw').fill(process.env['APP_PASSWORD']);"));
});

test('artifact limits evict older outputs while protecting the current run', async () => {
  const cwd = await workspace({ 'ferridriver.toml': 'artifactsRoot = "./artifacts"\nartifactsMaxBytes = 1\n' });
  const write = name => cli(['--config', 'ferridriver.toml', '--json', '-e',
    `await artifacts.writeBytes('${name}', new Array(64).fill(1)); return '${name}';`], { cwd });
  passed(await write('first.bin'));
  assert.equal((await readFile(join(cwd, 'artifacts/first.bin'))).length, 64);
  passed(await write('second.bin'));
  assert.equal((await readFile(join(cwd, 'artifacts/second.bin'))).length, 64);
  await assert.rejects(() => readFile(join(cwd, 'artifacts/first.bin')), error => error.code === 'ENOENT');
});
