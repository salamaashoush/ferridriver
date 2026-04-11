#!/usr/bin/env node
/**
 * ferridriver-test CLI -- runs E2E, component, and BDD tests using the Rust engine.
 *
 * Compiled to dist/cli.js via `bun run build:cli`. User TS files (config,
 * tests, step definitions) are loaded at runtime via jiti (Node) or
 * native import (Bun).
 */

import { defineCommand, defineArgs, runMain, withCompletions } from 'clap-ts';
import type { CommandDef } from 'clap-ts';

import { TestRunner } from '@ferridriver/core';
import type { Page } from '@ferridriver/core';
import { _setCurrentFile, _runWithFile, _drainTests, _hasOnly, _setCtMountFactory, _setRunner } from './test.js';
import type { MountFunction } from './test.js';
import { resolve, relative } from 'path';
import { existsSync, statSync } from 'fs';

import type { FerridriverTestConfig } from './config.js';

// ---- TS file loader: jiti on Node, native on Bun ----

let _importTs: (path: string) => Promise<any>;

if (typeof globalThis.Bun !== 'undefined') {
  _importTs = (path: string) => import(path);
} else {
  const { createJiti } = await import('jiti');
  const jiti = createJiti(import.meta.url, {
    fsCache: true,        // cache transpiled files to node_modules/.cache/jiti
    moduleCache: true,    // cache loaded modules in memory
    interopDefault: true, // auto-extract default exports
  });
  _importTs = (path: string) => jiti.import(path);
}

// ---- Config file loading ----

const CONFIG_CANDIDATES = [
  'ferridriver.config.ts',
  'ferridriver.config.js',
  'ferridriver.config.mjs',
  'ferridriver.config.mts',
];

async function loadConfig(explicitPath?: string): Promise<FerridriverTestConfig> {
  const candidates = explicitPath
    ? [resolve(explicitPath)]
    : CONFIG_CANDIDATES.map((f) => resolve(f));

  for (const path of candidates) {
    if (!existsSync(path)) continue;
    try {
      const mod = await _importTs(path);
      return mod.default ?? mod;
    } catch (e: any) {
      if (explicitPath) {
        console.error(`Failed to load config ${path}: ${e.message}`);
        process.exit(1);
      }
    }
  }
  return {};
}

/** Merge config file values with CLI arg overrides. CLI always wins. */
function mergeConfig(fileConfig: FerridriverTestConfig, cliArgs: Record<string, any>): Record<string, any> {
  const config: Record<string, any> = {};

  // Flatten file config's `use` block into top-level config (Playwright convention).
  if (fileConfig.use) {
    if (fileConfig.use.browserName) config.browser = fileConfig.use.browserName;
    if (fileConfig.use.headless !== undefined) config.headed = !fileConfig.use.headless;
    if (fileConfig.use.viewport) {
      config.viewportWidth = fileConfig.use.viewport.width;
      config.viewportHeight = fileConfig.use.viewport.height;
    }
    if (fileConfig.use.locale) config.locale = fileConfig.use.locale;
    if (fileConfig.use.colorScheme) config.colorScheme = fileConfig.use.colorScheme;
    if (fileConfig.use.isMobile !== undefined) config.isMobile = fileConfig.use.isMobile;
    if (fileConfig.use.hasTouch !== undefined) config.hasTouch = fileConfig.use.hasTouch;
    if (fileConfig.use.baseURL) config.baseUrl = fileConfig.use.baseURL;
    if (fileConfig.use.storageState) config.storageState = typeof fileConfig.use.storageState === 'string' ? fileConfig.use.storageState : undefined;
    if (fileConfig.use.channel) config.channel = fileConfig.use.channel;
  }

  // Map file config top-level fields.
  if (fileConfig.workers !== undefined) config.workers = fileConfig.workers;
  if (fileConfig.retries !== undefined) config.retries = fileConfig.retries;
  if (fileConfig.timeout !== undefined) config.timeout = fileConfig.timeout;
  if (fileConfig.forbidOnly) config.forbidOnly = true;
  if (fileConfig.outputDir) config.outputDir = fileConfig.outputDir;
  if (fileConfig.reporter) {
    const reporters = Array.isArray(fileConfig.reporter) ? fileConfig.reporter : [fileConfig.reporter];
    config.reporter = reporters.map((r) => (typeof r === 'string' ? r : r[0]));
  }
  if (fileConfig.projects) config.projects = fileConfig.projects;
  if (fileConfig.testMatch) config.testMatch = fileConfig.testMatch;
  if (fileConfig.testIgnore) config.testIgnore = fileConfig.testIgnore;
  if (fileConfig.testDir) config.testDir = fileConfig.testDir;
  if (fileConfig.webServer) config.webServer = fileConfig.webServer;

  // CLI args override everything.
  for (const [key, value] of Object.entries(cliArgs)) {
    if (value !== undefined && value !== false && value !== null) {
      config[key] = value;
    }
  }

  return config;
}

/** Map CLI args to runner config overrides. Single source of truth — no duplication. */
function buildCliOverrides(args: Record<string, any>): Record<string, any> {
  const o: Record<string, any> = {};
  if (args.workers !== undefined) o.workers = args.workers;
  if (args.retries !== undefined) o.retries = args.retries;
  if (args.timeout !== undefined) o.timeout = args.timeout;
  if (args.headed) o.headed = true;
  if (args.grep) o.grep = args.grep;
  if (args['grep-invert']) o.grepInvert = args['grep-invert'];
  if (args.shard) o.shard = args.shard;
  if (args.tag) o.tag = args.tag;
  if (args.output) o.outputDir = args.output;
  if (args.profile) o.profile = args.profile;
  if (args.backend) o.backend = args.backend;
  if (args.browser) {
    o.browser = args.browser;
    if (!args.backend) {
      if (o.browser === 'firefox') o.backend = 'bidi';
      else if (o.browser === 'webkit') o.backend = 'webkit';
    }
  }
  if (args.reporter) o.reporter = [args.reporter];
  if (args['update-snapshots']) o.updateSnapshots = true;
  if (args['forbid-only']) o.forbidOnly = true;
  if (args['last-failed']) o.lastFailed = true;
  if (args.video) o.video = args.video;
  if (args.trace) o.trace = args.trace;
  if (args['storage-state']) o.storageState = args['storage-state'];
  if (args['web-server-dir'] || args['web-server-cmd']) {
    o.webServer = {
      staticDir: args['web-server-dir'] || undefined,
      command: args['web-server-cmd'] || undefined,
      url: args['web-server-url'] || undefined,
    };
  }
  if (args.watch) o.watch = true;
  if (args.verbose) { o.verbose = 1; if (!args.debug) o.debug = '*'; }
  if (args.debug) o.debug = args.debug;
  return o;
}

// ---- Glob abstraction: use Bun.Glob if available, fall back to node:fs glob ----

async function* scanGlob(pattern: string, cwd: string): AsyncIterable<string> {
  if (typeof globalThis.Bun !== 'undefined') {
    const g = new globalThis.Bun.Glob(pattern);
    yield* g.scan({ cwd, absolute: true });
  } else {
    const { glob } = await import('fs/promises');
    yield* glob(pattern, { cwd });
  }
}

// ---- Shared arg groups (matches Rust TestArgs/BddArgs patterns) ----

const runnerArgs = defineArgs({
  workers: {
    type: 'number',
    short: 'j',
    description: 'Number of parallel workers',
  },
  retries: {
    type: 'number',
    description: 'Retry failed tests N times',
  },
  timeout: {
    type: 'number',
    description: 'Test timeout in milliseconds',
  },
  headed: {
    type: 'boolean',
    description: 'Run browser in headed mode (visible window)',
  },
  grep: {
    type: 'string',
    short: 'g',
    description: 'Only run tests matching regex pattern',
  },
  'grep-invert': {
    type: 'string',
    description: 'Exclude tests matching regex pattern',
  },
  shard: {
    type: 'string',
    description: 'Shard: current/total (e.g., "1/3")',
  },
  tag: {
    type: 'string',
    description: 'Tag filter',
  },
  output: {
    type: 'string',
    valueName: 'DIR',
    description: 'Output directory for reports and artifacts',
    valueHint: 'filePath',
  },
  profile: {
    type: 'string',
    description: 'Configuration profile to apply',
  },
  'web-server-dir': {
    type: 'string',
    valueName: 'DIR',
    description: 'Serve a static directory as the test server (sets base_url automatically)',
    valueHint: 'filePath',
  },
  'web-server-cmd': {
    type: 'string',
    description: 'Start a dev server command before tests (requires --web-server-url)',
  },
  'web-server-url': {
    type: 'string',
    description: 'URL to wait for when using --web-server-cmd',
  },
  backend: {
    type: 'string',
    description: 'Browser backend protocol',
    valueParser: ['cdp-pipe', 'cdp-raw', 'webkit', 'bidi'],
  },
  browser: {
    type: 'string',
    description: 'Browser product to launch (sets default backend)',
    valueParser: ['chromium', 'firefox', 'webkit'],
  },
  reporter: {
    type: 'string',
    description: 'Test reporter',
    valueParser: ['terminal', 'junit', 'json'],
  },
  'update-snapshots': {
    type: 'boolean',
    description: 'Update snapshot files',
  },
  list: {
    type: 'boolean',
    description: 'List discovered tests without running them',
  },
  'forbid-only': {
    type: 'boolean',
    description: 'Fail if test.only() is found (CI safety net)',
  },
  'last-failed': {
    type: 'boolean',
    description: 'Re-run only previously failed tests (from @rerun.txt)',
  },
  video: {
    type: 'string',
    description: 'Record video: off, on, retain-on-failure',
  },
  trace: {
    type: 'string',
    description: 'Record trace: off, on, retain-on-failure, on-first-retry',
  },
  'storage-state': {
    type: 'string',
    valueName: 'PATH',
    description: 'Path to storage state JSON (pre-authenticated session)',
    valueHint: 'filePath',
  },
  watch: {
    type: 'boolean',
    short: 'w',
    description: 'Watch mode: re-run tests on file changes',
  },
  verbose: {
    type: 'boolean',
    short: 'v',
    description: 'Verbose output (debug-level logging)',
  },
  debug: {
    type: 'string',
    description: 'Debug categories: cdp, steps, action, worker, fixture (comma-separated)',
  },
  config: {
    type: 'string',
    short: 'c',
    valueName: 'PATH',
    description: 'Path to config file (default: auto-detect ferridriver.config.ts)',
    valueHint: 'filePath',
  },
});

// ---- Path normalization: directories become globs ----

function normalizePaths(paths: string[], suffix: string): string[] {
  return paths.map((p) => {
    try {
      if (statSync(resolve(p)).isDirectory()) {
        return `${p.replace(/\/+$/, '')}/${suffix}`;
      }
    } catch { /* not a path, use as-is */ }
    return p;
  });
}

// ---- File discovery ----

async function collectGlob(pattern: string, cwd: string): Promise<string[]> {
  const results: string[] = [];
  for await (const file of scanGlob(pattern, cwd)) {
    const abs = resolve(file);
    if (!abs.includes('node_modules')) results.push(abs);
  }
  return results;
}

async function discoverFiles(files: string[], patterns: string[]): Promise<string[]> {
  if (files.length > 0) return files.map((f) => resolve(f));
  const results = await Promise.all(patterns.map((p) => collectGlob(p, process.cwd())));
  return results.flat().sort();
}

// ---- CT: resolve framework adapter ----

async function resolveCtAdapter(
  ctFramework: string | undefined,
  ctRegisterSource: string | undefined,
): Promise<{
  registerSourcePath: string;
  frameworkPlugin: (() => Promise<any>) | null;
}> {
  if (ctRegisterSource) {
    return { registerSourcePath: resolve(ctRegisterSource), frameworkPlugin: null };
  }

  const fw = ctFramework || 'react';
  const pkg = `@ferridriver/ct-${fw}`;

  const { createRequire } = await import('module');
  const projectRequire = createRequire(resolve('package.json'));

  const candidates = [
    () => projectRequire.resolve(pkg),
    () => pkg,
    () => resolve(`packages/ct-${fw}/src/index.mjs`),
  ];

  for (const getPath of candidates) {
    try {
      const resolvedPath = getPath();
      const adapter = await import(resolvedPath);
      return {
        registerSourcePath: adapter.registerSourcePath,
        frameworkPlugin: adapter.vitePlugin || null,
      };
    } catch {}
  }

  throw new Error(
    `Cannot find CT adapter for "${fw}". Install: npm i ${pkg}\n` +
    `Or specify: --register-source path/to/registerSource.mjs`,
  );
}

// ---- CT: wire mount() into test fixtures ----

function setupCtMount() {
  _setCtMountFactory((page: Page): MountFunction => {
    return async (component, options = {}) => {
      const props = options.props || {};
      const hooksConfig = options.hooksConfig || {};

      const componentRef = component?.__pw_type === 'importRef'
        ? component
        : { id: typeof component === 'string' ? component : component?.name || 'default' };

      const payload = JSON.stringify({
        component: componentRef,
        options: { props, hooksConfig },
      });

      await page.evaluate(`(async () => {
        const data = JSON.parse(${JSON.stringify(payload)});
        const root = document.getElementById('root') || document.getElementById('app');
        if (!root) throw new Error('No #root or #app element');
        if (!window.__ferriMount) throw new Error('__ferriMount not loaded');
        await window.__ferriMount(data.component, root, data.options);
      })()`);
    };
  });
}

// ---- BDD runner ----

async function discoverStepFiles(stepsGlobs: string[]): Promise<string[]> {
  const defaults = ['steps/**/*.ts', 'steps/**/*.js', 'step_definitions/**/*.ts', 'step_definitions/**/*.js'];
  const raw = stepsGlobs.length > 0
    ? [...normalizePaths(stepsGlobs, '**/*.ts'), ...normalizePaths(stepsGlobs, '**/*.js')]
    : defaults;

  const results = await Promise.all(raw.map((p) => collectGlob(p, process.cwd())));
  return results.flat().sort();
}

// ---- E2E test runner (shared by default and ct modes) ----

async function runTests(config: Record<string, any>, testFiles: string[], ctMode: boolean, featureFiles: string[] = [], stepFiles: string[] = []) {
  let viteProcess: any = null;

  if (ctMode) {
    const { spawn } = await import('child_process');
    const port = 3100 + Math.floor(Math.random() * 1000);

    const viteCmd = (await import('fs')).existsSync(resolve('node_modules/.bin/vite'))
      ? resolve('node_modules/.bin/vite')
      : 'bunx';
    const viteArgs = viteCmd.endsWith('vite')
      ? ['--port', String(port), '--host', '127.0.0.1']
      : ['--bun', 'vite', '--port', String(port), '--host', '127.0.0.1'];

    viteProcess = spawn(viteCmd, viteArgs, {
      cwd: resolve('.'),
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    const viteUrl = await new Promise<string>((resolveUrl, reject) => {
      const timeout = setTimeout(() => reject(new Error('Vite startup timeout (30s)')), 30000);
      const onData = (chunk: Buffer) => {
        const line = chunk.toString();
        const match = line.match(/http:\/\/127\.0\.0\.1:\d+/);
        if (match) {
          clearTimeout(timeout);
          resolveUrl(match[0]);
        }
      };
      viteProcess.stdout?.on('data', onData);
      viteProcess.stderr?.on('data', onData);
      viteProcess.on('error', (e: Error) => { clearTimeout(timeout); reject(e); });
    });

    const warmStart = Date.now();
    for (let i = 0; i < 60; i++) {
      try {
        const resp = await fetch(viteUrl);
        if (resp.ok) {
          const html = await resp.text();
          const scriptMatch = html.match(/src="([^"]+\.(?:tsx?|jsx?|mts|mjs))"/);
          if (scriptMatch) {
            await fetch(`${viteUrl}${scriptMatch[1]}`);
          }
          break;
        }
      } catch {}
      await new Promise(r => setTimeout(r, 200));
    }
    console.log(`[ct] Vite warmed in ${Date.now() - warmStart}ms`);

    config.baseUrl = viteUrl;
    if (!config.workers) config.workers = 4;
    setupCtMount();
    console.log(`[ct] Serving at ${viteUrl}`);
  }

  // Normalize webServer to array for NAPI.
  if (config.webServer && !Array.isArray(config.webServer)) {
    config.webServer = [config.webServer];
  }

  const runner = await TestRunner.create(config);
  _setRunner(runner);

  // Load all test files and step definitions in parallel.
  // Each file import runs within its own AsyncLocalStorage context
  // so test registrations get the correct file path.
  // All files load in parallel — E2E test files get per-file context via AsyncLocalStorage,
  // step files register on the shared runner via globalThis.__ferridriver.runner.
  await Promise.all([
    ...testFiles.map(file =>
      _runWithFile(relative(process.cwd(), file), () => _importTs(file)),
    ),
    ...stepFiles.map(f => _importTs(f)),
  ]);

  const tests = _drainTests();
  for (const t of tests) runner.registerTest(t.meta, t.body);

  if (tests.length === 0 && featureFiles.length === 0) {
    console.log('  No tests found.');
    if (viteProcess) viteProcess.kill();
    process.exit(0);
  }

  // Run — feature files passed to Rust for parsing/translation into the same plan.
  const summary = await runner.run(featureFiles.length > 0 ? featureFiles : undefined);

  if (viteProcess) viteProcess.kill();
  // Force exit — NAPI native addon may hold browser process handles that prevent
  // clean shutdown. process.exit() is the correct behavior here (same as Playwright).
  const exitCode = summary.failed > 0 ? 1 : 0;
  process.exit(exitCode);
}

// ---- Commands ----

const testCommand = defineCommand({
  meta: {
    name: 'test',
    description: 'Run tests (.spec.ts, .feature, or mixed)',
    aliases: ['e2e', 'bdd', 'run'],
  },
  args: {
    ...runnerArgs,
    // BDD-specific args
    steps: {
      type: 'string',
      action: 'append',
      valueName: 'GLOB',
      description: 'Step definition file glob patterns',
      valueHint: 'filePath',
    },
    tags: {
      type: 'string',
      short: 't',
      description: 'BDD tag expression filter (e.g., "@smoke and not @wip")',
    },
    strict: {
      type: 'boolean',
      description: 'Treat undefined/pending BDD steps as errors',
    },
    order: {
      type: 'string',
      description: 'BDD scenario order: "defined" (default) or "random" / "random:SEED"',
    },
    language: {
      type: 'string',
      description: 'Gherkin keyword language (e.g., "fr", "de")',
    },
    files: { type: 'positional', valueName: 'FILES', trailingVarArg: true },
  },
  async run({ args }) {
    const fileConfig = await loadConfig(args.config);
    const overrides = buildCliOverrides(args);
    const config = mergeConfig(fileConfig, overrides);

    // Wire BDD config from CLI args.
    if (args.tags) config.tags = args.tags;
    if (args.strict) config.strict = true;
    if (args.order) config.order = args.order;
    if (args.language) config.language = args.language;

    // Discover all files — use testMatch from config, or default patterns.
    const fileList = (args.files as string[] | undefined) ?? [];
    const defaultPatterns = ['**/*.spec.ts', '**/*.test.ts', '**/*.feature'];
    const patterns = (config.testMatch as string[] | undefined) ?? defaultPatterns;
    const allFiles = await discoverFiles(fileList, patterns);

    const testFiles = allFiles.filter(f => /\.(spec|test)\.[tj]sx?$/.test(f));
    const featureFiles = allFiles.filter(f => f.endsWith('.feature'));

    if (testFiles.length === 0 && featureFiles.length === 0) {
      console.log('  No test files found.');
      process.exit(0);
    }

    // Load step definitions if we have feature files.
    const stepsGlobs = (args.steps as string[] | undefined) ?? [];
    const stepFiles = featureFiles.length > 0 ? await discoverStepFiles(stepsGlobs) : [];

    await runTests(config, testFiles, false, featureFiles, stepFiles);
  },
});

const ctCommand = defineCommand({
  meta: {
    name: 'ct',
    description: 'Run component tests with Vite dev server',
    aliases: ['component'],
  },
  args: {
    ...runnerArgs,
    framework: {
      type: 'string',
      description: 'UI framework for component testing',
      valueParser: ['react', 'vue', 'svelte', 'solid'],
      default: 'react',
    },
    'register-source': {
      type: 'string',
      valueName: 'PATH',
      description: 'Custom component adapter source path',
      valueHint: 'filePath',
    },
    files: { type: 'positional', valueName: 'FILES', trailingVarArg: true },
  },
  async run({ args }) {
    const fileConfig = await loadConfig(args.config);
    const fileList = (args.files as string[] | undefined) ?? [];
    const testFiles = await discoverFiles(fileList, [
      '**/*.ct.ts', '**/*.ct.tsx', '**/*.ct.spec.ts', '**/*.ct.spec.tsx',
    ]);
    if (testFiles.length === 0) {
      console.log('  No component test files found.');
      process.exit(0);
    }
    const config = mergeConfig(fileConfig, buildCliOverrides(args));
    await runTests(config, testFiles, true);
  },
});


const codegenCommand = defineCommand({
  meta: {
    name: 'codegen',
    description: 'Record user interactions and generate test code',
  },
  args: {
    url: { type: 'positional', valueName: 'URL', required: true },
    language: {
      type: 'string',
      short: 'l',
      description: 'Output language: rust, typescript (ts), gherkin (bdd)',
      default: 'rust',
    },
    output: {
      type: 'string',
      short: 'o',
      description: 'Write generated code to file instead of stdout',
    },
    viewport: {
      type: 'string',
      description: 'Viewport size (WxH, e.g. "1280x720")',
    },
  },
  async run({ args }) {
    const { Codegen } = await import('ferridriver');

    const config: Record<string, any> = { url: args.url as string };
    if (args.language) config.language = args.language;
    if (args.output) config.outputFile = args.output;
    if (args.viewport) {
      const [w, h] = (args.viewport as string).split('x').map(Number);
      if (w && h) {
        config.viewportWidth = w;
        config.viewportHeight = h;
      }
    }

    await Codegen.run(config);
  },
});

// ---- Root command ----

const installCommand = defineCommand({
  meta: {
    name: 'install',
    description: 'Install browsers for automation',
  },
  args: defineArgs({
    'with-deps': {
      type: 'boolean',
      description: 'Also install system dependencies (fonts, libs)',
      default: false,
    },
  }),
  positionals: {
    browser: {
      type: 'string',
      description: 'Browser to install',
      default: 'chromium',
    },
  },
  async run({ args, positionals }) {
    const { installChromium, installSystemDeps, getBrowserCacheDir } = await import('@ferridriver/core');
    const browser = positionals.browser || 'chromium';
    if (!['chromium', 'chrome', 'firefox'].includes(browser)) {
      console.error(`Unsupported browser: ${browser}. Supported: chromium, firefox.`);
      process.exit(1);
    }
    console.log(`Browser cache: ${getBrowserCacheDir()}`);
    if (args['with-deps']) {
      console.log('Installing system dependencies...');
      await installSystemDeps();
      console.log('System dependencies installed.');
    }
    console.log('Installing Chromium...');
    const path = await installChromium();
    console.log(`Chromium installed: ${path}`);
  },
});

const root = defineCommand({
  meta: {
    name: 'ferridriver-test',
    version: '0.2.0',
    description: 'High-performance E2E, component, and BDD test runner',
    about: 'Runs tests using the ferridriver Rust engine with Playwright-compatible API',
    afterHelp:
      'Examples:\n' +
      '  ferridriver-test test                                    # Run all .spec.ts + .feature\n' +
      '  ferridriver-test test --headed -j 4                      # 4 workers, headed browser\n' +
      '  ferridriver-test test tests/smoke.spec.ts                # Run specific E2E test\n' +
      '  ferridriver-test test tests/features/*.feature --tags "@smoke"  # BDD with tag filter\n' +
      '  ferridriver-test test tests/ --steps steps/              # Mixed E2E + BDD\n' +
      '  ferridriver-test ct --framework react                    # Component tests with React\n' +
      '  ferridriver-test codegen https://example.com             # Record interactions as test code\n' +
      '  ferridriver-test install --with-deps                     # Install Chromium + system deps',
  },
  subCommands: {
    test: testCommand,
    ct: ctCommand,
    codegen: codegenCommand,
    install: installCommand,
  },
});

await runMain(withCompletions(root));
