import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, repo, runtimeProbe } from './support.mjs';

async function resolve(files = {}, options = {}) {
  const { results, cwd } = await runtimeProbe([{ op: 'config-layers', request: {
    cwd: 'repo', user: 'user', inherit: true, ...options,
  } }], { 'repo/.git/keep': '', 'repo/pkg/keep': '', 'user/ferridriver/keep': '', ...files });
  return { ...observation(results[0]), root: cwd };
}
const kinds = result => result.layers.map(layer => layer.kind);

test('user and project configuration contribute distinct values', async () => {
  const r = await resolve({
    'user/ferridriver/config.yaml': 'mcp:\n  server:\n    name: acme-user\n  browser:\n    headless: true\n',
    'repo/ferridriver.toml': '[test]\nworkers = 7\n',
  });
  assert.equal(r.serverName, 'acme-user');
  assert.equal(r.headless, true);
  assert.equal(r.config.test.workers, 7);
  assert.deepEqual(kinds(r), ['user', 'cwd']);
});

test('the winning scalar retains its file provenance', async () => {
  const r = await resolve({
    'user/ferridriver/config.toml': '[mcp.server]\nname = "from-user"\n',
    'repo/ferridriver.toml': '[mcp.server]\nname = "from-project"\n',
  });
  assert.equal(r.serverName, 'from-project');
  assert.deepEqual(r.provenance['mcp.server.name'], { from: 'file', source: join(r.root, 'repo/ferridriver.toml') });
});

test('additive configuration arrays concatenate and deduplicate entries', async () => {
  const r = await resolve({
    'user/ferridriver/config.toml': 'extensions = ["/abs/acme.ts"]\n[mcp.browser]\nchromeArgs = ["--user-flag"]\n',
    'repo/ferridriver.toml': 'extensions = ["/abs/acme.ts", "/abs/repo.ts"]\n[mcp.browser]\nchromeArgs = ["--repo-flag"]\n',
  });
  assert.deepEqual(r.extensions, ['/abs/acme.ts', '/abs/repo.ts']);
  assert.deepEqual(r.chromeArgs, ['--user-flag', '--repo-flag']);
});

test('extension shorthand combines with paths and policy tables', async () => {
  const r = await resolve({
    'user/ferridriver/config.toml': 'extensions = ["/abs/acme.ts"]\n',
    'repo/ferridriver.toml': '[extensions]\npaths = ["/abs/repo.ts"]\n[extensions.policy]\ncommands = "argvOnly"\n',
  });
  assert.deepEqual(r.extensions, ['/abs/acme.ts', '/abs/repo.ts']);
  assert.equal(r.config.extensions.policy.commands, 'argvOnly');
});

test('relative paths anchor to the file declaring each value', async () => {
  const r = await resolve({
    'user/ferridriver/config.yaml': 'extensions:\n  - ./plugins/acme.ts\nscriptRoot: ./scripts\n',
    'repo/ferridriver.toml': '[test]\ntestDir = "./e2e"\n',
  });
  assert.deepEqual(r.extensions, [join(r.root, 'user/ferridriver/plugins/acme.ts')]);
  assert.equal(r.config.test.testDir, join(r.root, 'repo/e2e'));
});

test('package extension specifiers retain their declaring directory', async () => {
  const r = await resolve({ 'user/ferridriver/config.yaml': 'extensions:\n  - "@acme/ferridriver-acme"\n' });
  assert.deepEqual(r.extensions, ['@acme/ferridriver-acme']);
  assert.deepEqual(r.extensionSpecs, [{ spec: '@acme/ferridriver-acme', baseDir: join(r.root, 'user/ferridriver') }]);
});

test('document and module configuration both apply with module values on top', async () => {
  const r = await resolve({
    'repo/ferridriver.toml': 'extensions = ["./pkg"]\n[test]\ntimeout = 1234\n',
    'repo/ferridriver.config.ts': 'export default {};',
  }, { module: { test: { timeout: 4321, testDir: 'specs' } } });
  assert.ok(r.warnings.every(warning => !warning.message.includes('also present and ignored')));
  assert.equal(r.config.test.timeout, 4321);
  assert.equal(r.extensions.length, 1);
});

test('competing document formats report which sibling was shadowed', async () => {
  const r = await resolve({
    'repo/ferridriver.toml': '[test]\ntimeout = 1234\n',
    'repo/ferridriver.yaml': 'test:\n  timeout: 9999\n',
  });
  assert.equal(r.config.test.timeout, 1234);
  assert.ok(r.warnings.some(warning => warning.message.includes('also present and ignored') && warning.message.includes('ferridriver.yaml')));
});

test('tilde extension paths expand while instance templates remain intact', async () => {
  const r = await resolve({ 'repo/ferridriver.yaml': 'extensions:\n  - ~/plugins/acme/login.ts\n' });
  assert.ok(!r.extensions[0].startsWith('~'));
  assert.ok(r.extensions[0].endsWith('/plugins/acme/login.ts'));
  const template = await resolve({
    'repo/ferridriver.toml': '[mcp.browser.instances.staging]\ndiscoverProfile = "~/.box/profiles/${INSTANCE}"\n',
  });
  assert.equal(template.config.mcp.browser.instances.staging.discoverProfile, '~/.box/profiles/${INSTANCE}');
});

test('repository configuration applies from an unconfigured subdirectory', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[test]\nworkers = 3\n', 'repo/packages/web/keep': '' }, { cwd: 'repo/packages/web' });
  assert.equal(r.config.test.workers, 3);
  assert.deepEqual(kinds(r), ['project']);
});

test('nested package arrays can clear inherited values while retaining other defaults', async () => {
  const r = await resolve({
    'repo/ferridriver.toml': '[test]\nworkers = 9\nsteps = ["tests/steps/**/*.ts"]\n',
    'repo/pkg/ferridriver.toml': '[test]\nsteps = []\n',
  }, { cwd: 'repo/pkg' });
  assert.equal(r.config.test.workers, 9);
  assert.deepEqual(r.config.test.steps, []);
  assert.deepEqual(kinds(r), ['project', 'cwd']);
});

test('ancestor configuration applies outermost first and local overrides win last', async () => {
  const r = await resolve({
    'repo/ferridriver.toml': '[test]\nworkers = 1\ntimeout = 1000\n',
    'repo/a/ferridriver.toml': '[test]\nworkers = 2\n',
    'repo/a/b/ferridriver.toml': '[test]\nworkers = 3\n',
  }, { cwd: 'repo/a/b' });
  assert.equal(r.config.test.workers, 3);
  assert.equal(r.config.test.timeout, 1000);
  assert.equal(r.layers.length, 3);
  const local = await resolve({
    'repo/ferridriver.toml': '[mcp.browser]\nheadless = true\n',
    'repo/ferridriver.local.toml': '[mcp.browser]\nheadless = false\n',
  });
  assert.equal(local.headless, false);
  assert.equal(kinds(local).at(-1), 'local');
});

test('explicit configuration overrides user values and can disable inheritance', async () => {
  const files = {
    'user/ferridriver/config.toml': 'extensions = ["/abs/acme.ts"]\n[mcp.server]\nname = "user"\n',
    'repo/ci.toml': '[mcp.server]\nname = "ci"\n',
  };
  const r = await resolve(files, { explicit: 'repo/ci.toml' });
  assert.equal(r.serverName, 'ci');
  assert.deepEqual(r.extensions, ['/abs/acme.ts']);
  assert.equal(kinds(r).at(-1), 'explicit');
  const isolated = await resolve(files, { explicit: 'repo/ci.toml', inherit: false });
  assert.equal(isolated.serverName, 'ci');
  assert.deepEqual(isolated.extensions, []);
});

test('extended files contribute below their caller and cycles terminate', async () => {
  const r = await resolve({
    'repo/base.toml': '[mcp.server]\nname = "base"\n[mcp.browser]\nheadless = true\n',
    'repo/ferridriver.toml': 'extends = ["./base.toml"]\n[mcp.server]\nname = "child"\n',
  });
  assert.equal(r.serverName, 'child');
  assert.equal(r.headless, true);
  assert.deepEqual(kinds(r), ['extends', 'cwd']);
  const cycle = await resolve({
    'repo/ferridriver.toml': 'extends = ["./other.toml"]\n[test]\nworkers = 2\n',
    'repo/other.toml': 'extends = ["./ferridriver.toml"]\n[test]\nretries = 4\n',
  });
  assert.equal(cycle.config.test.workers, 2);
  assert.equal(cycle.config.test.retries, 4);
});

test('environment overrides beat files and retain their environment provenance', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[mcp.browser]\nheadless = false\nbackend = "cdp-pipe"\n' }, {
    env: {
      FERRIDRIVER_MCP__BROWSER__HEADLESS: 'true',
      FERRIDRIVER_MCP__BROWSER__BACKEND: 'cdp-raw',
      FERRIDRIVER_MCP__BROWSER__INSTANCE_ARGS_COMMAND: 'echo --from-env',
    },
  });
  assert.equal(r.headless, true);
  assert.equal(r.config.mcp.browser.backend, 'cdp-raw');
  assert.equal(r.config.mcp.browser.instanceArgsCommand.run, 'echo --from-env');
  assert.deepEqual(r.provenance['mcp.browser.headless'], { from: 'env', source: 'FERRIDRIVER_MCP__BROWSER__HEADLESS' });
});

test('single-segment runner environment variables are not document configuration keys', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[test]\nworkers = 5\n' }, {
    env: { FERRIDRIVER_WORKERS: '9', FERRIDRIVER_DEBUG: '1' },
  });
  assert.equal(r.config.test.workers, 5);
  assert.deepEqual(r.warnings, []);
});

test('legacy aliases parse while unknown keys produce a useful warning', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[mcp.browser]\nchrome_args = ["--x"]\n' });
  assert.deepEqual(r.chromeArgs, ['--x']);
  const typo = await resolve({ 'repo/ferridriver.toml': '[mcp.browser]\nchrom_args = ["--x"]\n' });
  assert.ok(typo.warnings.some(warning => warning.message.includes('chrom_args')));
});

test('MCP camelCase and legacy snake_case settings reach the typed configuration', async () => {
  const r = await resolve({ 'repo/ferridriver.toml':
    '[mcp.server]\nextraInstructions = "hello"\n[mcp.browser]\nchromeArgs = ["--x"]\nexecutablePath = "/bin/chrome"\ninstanceArgsCommand = "echo hi"\ncommandCacheTtl = 60\n' });
  assert.deepEqual(r.chromeArgs, ['--x']);
  assert.equal(r.config.mcp.browser.executablePath, '/bin/chrome');
  assert.equal(r.config.mcp.browser.instanceArgsCommand.run, 'echo hi');
  assert.equal(r.config.mcp.browser.commandCacheTtl, 60);
  assert.ok(r.instructions.includes('hello'));
  assert.deepEqual(r.warnings, []);
  const legacy = await resolve({ 'repo/ferridriver.toml':
    '[mcp.server]\nextra_instructions = "hello"\n[mcp.browser]\ninstance_discover_command = "echo ws"\ncommand_cache_ttl = 30\n' });
  assert.ok(legacy.instructions.includes('hello'));
  assert.equal(legacy.config.mcp.browser.instanceDiscoverCommand.run, 'echo ws');
  assert.equal(legacy.config.mcp.browser.commandCacheTtl, 30);
});

for (const [label, source, needles] of [
  ['missing extended file', 'extends = ["./nope.toml"]\n', ['nope.toml']],
  ['invalid backend', '[mcp.browser]\nbackend = "chrom-pipe"\n', ['chrom-pipe', 'cdp-pipe']],
  ['malformed TOML', '[mcp.browser\nheadless = true\n', ['invalid TOML']],
]) {
  test(`configuration rejects ${label} with a specific diagnostic`, async () => {
    await assert.rejects(() => resolve({ 'repo/ferridriver.toml': source }), error => {
      for (const needle of needles) assert.ok(error.message.includes(needle), error.message);
      return true;
    });
  });
}

test('empty configuration files participate and no files produce defaults', async () => {
  const r = await resolve({ 'user/ferridriver/config.yaml': '', 'repo/ferridriver.toml': '[test]\nworkers = 4\n' });
  assert.equal(r.config.test.workers, 4);
  const empty = await resolve({}, { cwd: '.' });
  assert.deepEqual(empty.layers, []);
  assert.equal(empty.serverName, 'ferridriver');
});

test('extension defaults lose only the keys a file explicitly overrides', async () => {
  const options = { user: null, defaults: [['pkg', { test: { testDir: 'from-extension', timeout: 12345 } }]] };
  const r = await resolve({}, options);
  assert.equal(r.config.test.testDir, 'from-extension');
  assert.equal(r.config.test.timeout, 12345);
  assert.equal(r.origins['test.timeout'], 'extension pkg');
  const override = await resolve({ 'repo/ferridriver.toml': '[test]\ntimeout = 999\n' }, options);
  assert.equal(override.config.test.timeout, 999);
  assert.equal(override.config.test.testDir, 'from-extension');
});

test('later extension defaults win and provenance names the package', async () => {
  const r = await resolve({}, { defaults: [['first', { test: { timeout: 1 } }], ['second', { test: { timeout: 2 } }]] });
  assert.equal(r.config.test.timeout, 2);
  assert.equal(r.origins['test.timeout'], 'extension second');
});

for (const [payload, needles] of [
  [{ test: { 'testIdAttribut': 'data-qa' } }, ['testIdAttribut', 'pkg']],
  [{ extensions: { policy: {} } }, ['extensions']],
  [{ bundler: { conditions: ['node'] } }, ['bundler']],
  [{ scripting: { allowEnv: ['HOME'] } }, ['scripting']],
  [{ test: { moduleAliases: { a: 'b' } } }, ['test.moduleAliases']],
]) {
  test(`extension defaults reject ${needles[0]}`, async () => {
    await assert.rejects(() => resolve({}, { defaults: [['pkg', payload]] }), error => {
      for (const needle of needles) assert.ok(error.message.includes(needle), error.message);
      return true;
    });
  });
}

const used = result => result.config.test.browser.use;

test('device descriptors seed every configured browser context field', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[test.browser.use]\ndevice = "iPhone 15"\n' });
  const use = used(r);
  assert.ok(use.userAgent.includes('iPhone'));
  assert.equal(use.isMobile, true);
  assert.equal(use.hasTouch, true);
  assert.equal(use.deviceScaleFactor, 3);
  assert.equal(use.defaultBrowserType, 'webkit');
  assert.deepEqual(use.viewport, { width: 393, height: 659 });
  assert.deepEqual(use.screen, { width: 393, height: 852 });
});

test('explicit context values beat device defaults in the same or a higher layer', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[test.browser.use]\ndevice = "iPhone 15"\nhasTouch = false\nuserAgent = "mine"\n' });
  assert.equal(used(r).hasTouch, false);
  assert.equal(used(r).userAgent, 'mine');
  assert.equal(used(r).isMobile, true);
  const layered = await resolve({
    'user/ferridriver/config.toml': '[test.browser.use]\ndevice = "iPhone 15"\n',
    'repo/ferridriver.toml': '[test.browser.use]\nuserAgent = "repo-agent"\n',
  });
  assert.equal(used(layered).userAgent, 'repo-agent');
  assert.equal(used(layered).isMobile, true);
});

test('unknown devices preserve the name without inventing descriptor values', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[test.browser.use]\ndevice = "Nokia 3310"\n' });
  assert.equal(used(r).device, 'Nokia 3310');
  assert.ok(used(r).userAgent == null);
  assert.ok(used(r).viewport == null);
  assert.equal(r.useViewportPresent, false);
});

test('top-level use aliases reach context settings and yield to browser use', async () => {
  const r = await resolve({ 'repo/ferridriver.toml': '[test.use]\nlocale = "fr-FR"\ndevice = "Pixel 5"\n' });
  assert.equal(used(r).locale, 'fr-FR');
  assert.equal(used(r).deviceScaleFactor, 2.75);
  assert.ok(r.warnings.every(warning => !warning.message.includes('use')));
  const own = await resolve({ 'repo/ferridriver.toml': '[test.use]\nlocale = "fr-FR"\n[test.browser.use]\nlocale = "de-DE"\n' });
  assert.equal(used(own).locale, 'de-DE');
});

test('project device options select their engine without undoing headless mode', async () => {
  const r = await resolve({ 'repo/ferridriver.toml':
    '[test.browser]\nheadless = true\n[[test.projects]]\nname = "phone"\n[test.projects.use]\ndevice = "iPhone 15"\n' });
  assert.equal(r.projects[0].browser.use.isMobile, true);
  assert.equal(r.projects[0].browser.browser, 'webkit');
  assert.equal(r.projects[0].browser.backend, 'webkit');
  assert.equal(r.projects[0].browser.headless, true);
});

test('explicit browser selection beats the device engine while keeping its context settings', async () => {
  const named = await resolve({ 'repo/ferridriver.toml': '[test.use]\ndevice = "iPhone 15"\nbrowserName = "chromium"\n' });
  assert.equal(named.effective.browser.browser, 'chromium');
  const selected = await resolve({ 'repo/ferridriver.toml':
    '[test.browser]\nbrowser = "firefox"\nbackend = "bidi"\n[test.use]\ndevice = "iPhone 15"\n' });
  assert.equal(selected.effective.browser.browser, 'firefox');
  assert.equal(selected.effective.browser.use.isMobile, true);
});

test('null viewport disables emulation while an empty size table is invalid', async () => {
  await assert.rejects(() => resolve({ 'repo/ferridriver.toml': '[test.use]\nviewport = {}\n' }), /width/);
  const r = await resolve({ 'repo/ferridriver.yaml': 'test:\n  use:\n    viewport: null\n' });
  assert.equal(used(r).viewport, null);
  assert.equal(r.useViewportPresent, true);
});

test('use options settle runner recording, base URL, and action timeouts', async () => {
  const r = await resolve({ 'repo/ferridriver.toml':
    '[test]\nbaseUrl = "http://from-top-level"\n[test.use]\nbaseURL = "http://from-use"\ntrace = "on-first-retry"\nvideo = "retain-on-failure"\nscreenshot = "on"\nactionTimeout = 1500\nnavigationTimeout = 9000\n' });
  assert.equal(r.effective.baseUrl, 'http://from-use');
  assert.equal(r.effective.trace, 'on-first-retry');
  assert.equal(r.effective.video.mode, 'retain-on-failure');
  assert.equal(r.effective.screenshot.mode, 'on');
  assert.equal(r.effective.browser.use.actionTimeout, 1500);
  assert.equal(r.effective.browser.use.navigationTimeout, 9000);
});

test('recording option objects retain flags and dimensions beyond their mode', async () => {
  const r = await resolve({ 'repo/ferridriver.yaml':
    'test:\n  use:\n    trace: { mode: on, snapshots: false, sources: false }\n    video: { mode: on, size: { width: 640, height: 480 } }\n    screenshot: { mode: only-on-failure, fullPage: false }\n' });
  const cfg = r.effective;
  assert.equal(cfg.trace, 'on');
  assert.equal(cfg.browser.use.trace.snapshots, false);
  assert.equal(cfg.browser.use.trace.sources, false);
  assert.ok(cfg.browser.use.trace.screenshots == null);
  assert.equal(cfg.video.mode, 'on');
  assert.equal(cfg.video.width, 640);
  assert.equal(cfg.video.height, 480);
  assert.equal(cfg.screenshot.mode, 'only-on-failure');
  assert.equal(cfg.screenshot.fullPage, false);
});

test('misspelled keys inside recording options remain visible in errors', async () => {
  await assert.rejects(() => resolve({ 'repo/ferridriver.yaml':
    'test:\n  use:\n    trace: { mode: on, snapshotz: false }\n' }), /snapshotz/);
});

test('screenshot mode overrides the legacy boolean and keeps it consistent', async () => {
  const off = await resolve({ 'repo/ferridriver.toml': '[test]\nscreenshotOnFailure = false\n' });
  assert.equal(off.effective.screenshot.mode, 'off');
  const on = await resolve({ 'repo/ferridriver.toml': '[test]\nscreenshotOnFailure = false\n[test.use]\nscreenshot = "on"\n' });
  assert.equal(on.effective.screenshot.mode, 'on');
  assert.equal(on.effective.screenshotOnFailure, true);
});

test('shared browser values reach both hosts while section overrides retain precedence', async () => {
  const r = await resolve({ 'repo/ferridriver.yaml':
    'browser:\n  backend: cdp-raw\n  headless: true\ntest:\n  browser:\n    backend: webkit\n' });
  assert.equal(r.config.mcp.browser.headless, true);
  assert.equal(r.config.test.browser.headless, true);
  assert.equal(r.config.mcp.browser.backend, 'cdp-raw');
  assert.equal(r.config.test.browser.backend, 'webkit');
  assert.ok(Object.hasOwn(r.provenance, 'mcp.browser.backend'));
});

test('shared viewport sizes and explicit nulls reach both hosts', async () => {
  const r = await resolve({ 'repo/ferridriver.yaml':
    'browser:\n  viewport: { width: 1600, height: 900 }\ntest:\n  browser:\n    viewport: { width: 800, height: 600 }\n' });
  assert.equal(r.mcpViewport.width, 1600);
  assert.equal(r.mcpViewport.height, 900);
  assert.equal(r.config.test.browser.viewport.width, 800);
  assert.equal(r.config.test.browser.viewport.height, 600);
  const disabled = await resolve({ 'repo/ferridriver.yaml': 'browser:\n  viewport: null\n' });
  assert.equal(disabled.mcpViewport, null);
  assert.equal(disabled.config.test.browser.viewport, null);
});

test('one startup cache preserves its first read while a new cache sees changed files', async () => {
  const { results } = await runtimeProbe([{ op: 'config-cached', path: 'ferridriver.toml', updated: '[test]\ntimeout = 9999\n' }], {
    'ferridriver.toml': '[test]\ntimeout = 1234\n',
  });
  const r = observation(results[0]);
  assert.equal(r.first.test.timeout, 1234);
  assert.equal(r.second.test.timeout, 1234);
  assert.equal(r.fresh.test.timeout, 9999);
});

test('trace and video policies preserve recording and retention for every retry mode', async () => {
  const cases = [
    ['off', 1, false, false, false], ['on', 1, true, true, true],
    ['retain-on-failure', 1, true, false, true],
    ['on-first-retry', 1, false, true, true], ['on-first-retry', 2, true, true, true],
    ['on-all-retries', 1, false, true, true], ['on-all-retries', 2, true, true, true],
    ['retain-on-first-failure', 1, true, false, true], ['retain-on-first-failure', 2, false, false, true],
    ['retain-on-failure-and-retries', 1, true, false, true], ['retain-on-failure-and-retries', 2, true, true, true],
  ];
  const { results } = await runtimeProbe([{ op: 'config-contracts',
    modes: [...cases.map(([label, attempt]) => [label, attempt]), ['retry-with-trace', 1], ['retry-with-video', 1]],
  }]);
  const r = observation(results[0]);
  for (const [index, [label, attempt, record, pass, fail]] of cases.entries()) {
    const mode = r.modes[index];
    assert.equal(mode.recordTrace, record, `${label} @${attempt}`);
    assert.equal(mode.retainPass, pass, `${label} @${attempt}`);
    assert.equal(mode.retainFail, fail, `${label} @${attempt}`);
    assert.equal(mode.recordVideo, record, `${label} @${attempt}`);
    assert.equal(mode.eagerVideo, pass, `${label} @${attempt}`);
  }
  assert.equal(r.modes[cases.length].trace, 'on-first-retry');
  assert.equal(r.modes[cases.length + 1].video, 'on-first-retry');
});

function declaredKeys(source, name) {
  const start = source.indexOf(`interface ${name} {`);
  assert.ok(start >= 0, `${name} must be declared`);
  const body = source.slice(start);
  const ends = ['\n}', '\n  }'].map(end => body.indexOf(end)).filter(index => index >= 0);
  assert.ok(ends.length > 0, `${name} must close`);
  return body.slice(0, Math.min(...ends)).split('\n').slice(1).map(line => line.trim())
    .filter(line => !line.startsWith('/*') && !line.startsWith('*') && !line.startsWith('//'))
    .map(line => line.split('?')[0]).filter(Boolean).sort();
}

test('extension and config-module authoring types declare exactly their allowed schema keys', async () => {
  const { results } = await runtimeProbe([{ op: 'config-contracts', modes: [] }]);
  const r = observation(results[0]);
  const extension = await readFile(join(repo, 'packages/ferridriver-extension/index.d.ts'), 'utf8');
  const config = await readFile(join(repo, 'packages/ferridriver-test/index.d.ts'), 'utf8');
  const testKeys = Object.keys(r.defaults.test).filter(key => key !== 'moduleAliases').sort();
  assert.deepEqual(declaredKeys(extension, 'TestConfigDefaults'), testKeys);
  assert.deepEqual(declaredKeys(extension, 'McpConfigDefaults'), Object.keys(r.defaults.mcp).sort());
  assert.deepEqual(declaredKeys(config, 'FerridriverTestConfig'), [...testKeys, ...r.aliases].sort());
});
