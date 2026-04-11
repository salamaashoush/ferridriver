/**
 * ferridriver-test CLI -- runs E2E, component, and BDD tests using the Rust engine.
 *
 * Entry points:
 *   - Node.js: src/bin.mjs (registers TS loader, then imports this file)
 *   - Bun: src/cli.ts (native TS support, no loader needed)
 *
 * E2E mode (default):
 *   ferridriver-test [files...] [--workers N] [--retries N] [--headed]
 *
 * Component testing mode:
 *   ferridriver-test ct [files...] [--framework react]
 *
 * BDD mode:
 *   ferridriver-test bdd [features...] [--steps steps/] [--tags "@smoke"]
 */

import { defineCommand, defineArgs, runMain, withCompletions } from 'clap-ts';
import type { CommandDef } from 'clap-ts';

import { TestRunner, BddRunner } from '@ferridriver/core';
import type { Page, BddRunnerConfig } from '@ferridriver/core';
import { _setCurrentFile, _drainTests, _hasOnly, _setCtMountFactory, _setRunner } from './test.js';
import type { MountFunction } from './test.js';
import { resolve, relative } from 'path';
import { existsSync, statSync } from 'fs';

import type { FerridriverTestConfig } from './config.js';

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
      const mod = await import(path);
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

  // CLI args override everything.
  for (const [key, value] of Object.entries(cliArgs)) {
    if (value !== undefined && value !== false && value !== null) {
      config[key] = value;
    }
  }

  return config;
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

async function runTests(config: Record<string, any>, testFiles: string[], ctMode: boolean) {
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

  const runner = await TestRunner.create(config);
  _setRunner(runner);
  const workerCount = runner.workerCount();

  for (const file of testFiles) {
    _setCurrentFile(relative(process.cwd(), file));
    await import(file);
  }

  const tests = _drainTests();
  if (tests.length === 0) {
    console.log('  No tests found.');
    if (viteProcess) viteProcess.kill();
    process.exit(0);
  }

  // Register all tests — core Rust runner handles all filtering
  // (only, skip, fixme, grep, tag, shard, last-failed).
  for (const t of tests) runner.registerTest(t.meta, t.body);

  // Rust reporters handle all terminal output (icons, colors, progress, summary).
  // TS CLI only needs the exit code from the summary.
  const summary = await runner.run();

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
    description: 'Run E2E tests (default when no subcommand)',
    aliases: ['e2e'],
  },
  args: {
    ...runnerArgs,
    files: { type: 'positional', valueName: 'FILES', trailingVarArg: true },
  },
  async run({ args }) {
    const fileConfig = await loadConfig(args.config);
    const fileList = (args.files as string[] | undefined) ?? [];
    const testFiles = await discoverFiles(fileList, ['**/*.spec.ts', '**/*.test.ts']);
    if (testFiles.length === 0) {
      console.log('  No test files found.');
      process.exit(0);
    }
    // Build CLI overrides.
    const cliOverrides: Record<string, any> = {};
    if (args.workers !== undefined) cliOverrides.workers = args.workers;
    if (args.retries !== undefined) cliOverrides.retries = args.retries;
    if (args.timeout !== undefined) cliOverrides.timeout = args.timeout;
    if (args.headed) cliOverrides.headed = true;
    if (args.grep) cliOverrides.grep = args.grep;
    if (args.backend) cliOverrides.backend = args.backend;
    if (args.browser) {
      cliOverrides.browser = args.browser;
      if (!args.backend) {
        if (cliOverrides.browser === 'firefox') cliOverrides.backend = 'bidi';
        else if (cliOverrides.browser === 'webkit') cliOverrides.backend = 'webkit';
      }
    }
    if (args.reporter) cliOverrides.reporter = [args.reporter];
    if (args['update-snapshots']) cliOverrides.updateSnapshots = true;
    if (args['forbid-only']) cliOverrides.forbidOnly = true;
    if (args['last-failed']) cliOverrides.lastFailed = true;
    if (args.video) cliOverrides.video = args.video;
    if (args.trace) cliOverrides.trace = args.trace;
    if (args['storage-state']) cliOverrides.storageState = args['storage-state'];
    if (args.watch) cliOverrides.watch = true;
    if (args.verbose) cliOverrides.verbose = 1;
    if (args.debug) cliOverrides.debug = args.debug;
    else if (args.verbose) cliOverrides.debug = '*';

    const config = mergeConfig(fileConfig, cliOverrides);
    await runTests(config, testFiles, false);
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
    const cliOverrides: Record<string, any> = {};
    if (args.workers !== undefined) cliOverrides.workers = args.workers;
    if (args.retries !== undefined) cliOverrides.retries = args.retries;
    if (args.timeout !== undefined) cliOverrides.timeout = args.timeout;
    if (args.headed) cliOverrides.headed = true;
    if (args.grep) cliOverrides.grep = args.grep;
    if (args.backend) cliOverrides.backend = args.backend;
    if (args.browser) {
      cliOverrides.browser = args.browser;
      if (!args.backend) {
        if (args.browser === 'firefox') cliOverrides.backend = 'bidi';
        else if (args.browser === 'webkit') cliOverrides.backend = 'webkit';
      }
    }
    if (args.reporter) cliOverrides.reporter = [args.reporter];
    if (args['update-snapshots']) cliOverrides.updateSnapshots = true;
    if (args['forbid-only']) cliOverrides.forbidOnly = true;
    if (args['last-failed']) cliOverrides.lastFailed = true;
    if (args.video) cliOverrides.video = args.video;
    if (args.trace) cliOverrides.trace = args.trace;
    if (args['storage-state']) cliOverrides.storageState = args['storage-state'];
    if (args.watch) cliOverrides.watch = true;
    if (args.verbose && !process.env.FERRIDRIVER_DEBUG) process.env.FERRIDRIVER_DEBUG = '*';

    const config = mergeConfig(fileConfig, cliOverrides);
    await runTests(config, testFiles, true);
  },
});

const bddCommand = defineCommand({
  meta: {
    name: 'bdd',
    description: 'Run BDD/Gherkin feature tests',
    aliases: ['features'],
  },
  args: {
    ...runnerArgs,
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
      description: 'Tag expression filter (e.g., "@smoke and not @wip")',
    },
    'dry-run': {
      type: 'boolean',
      description: 'Parse features and match steps without executing',
    },
    'fail-fast': {
      type: 'boolean',
      description: 'Stop on first failure',
    },
    'step-timeout': {
      type: 'number',
      valueName: 'MS',
      description: 'Timeout per step in milliseconds',
    },
    strict: {
      type: 'boolean',
      description: 'Treat undefined/pending steps as errors',
    },
    order: {
      type: 'string',
      description: 'Scenario order: "defined" (default) or "random" / "random:SEED"',
    },
    language: {
      type: 'string',
      description: 'Gherkin keyword language (e.g., "fr", "de")',
    },
    features: { type: 'positional', valueName: 'FEATURES', trailingVarArg: true },
  },
  async run({ args }) {
    const fileConfig = await loadConfig(args.config);
    const featureFiles = (args.features as string[] | undefined) ?? [];
    const featurePatterns = featureFiles.length > 0
      ? normalizePaths(featureFiles, '**/*.feature')
      : ['features/**/*.feature'];

    const stepsGlobs = (args.steps as string[] | undefined) ?? [];
    const stepFiles = await discoverStepFiles(stepsGlobs);

    // Merge file config with CLI overrides for BDD runner.
    const merged = mergeConfig(fileConfig, {
      workers: args.workers,
      retries: args.retries,
      timeout: args.timeout ?? args['step-timeout'],
      headed: args.headed,
      grep: args.grep,
      backend: args.backend || (args.browser === 'firefox' ? 'bidi' : args.browser === 'webkit' ? 'webkit' : undefined),
      browser: args.browser,
      reporter: args.reporter ? [args.reporter] : undefined,
      video: args.video,
      trace: args.trace,
      'storage-state': args['storage-state'],
      watch: args.watch,
    });

    const bddConfig: BddRunnerConfig = {
      features: featurePatterns,
      tags: args.tags || undefined,
      workers: merged.workers,
      timeout: merged.timeout,
      retries: merged.retries,
      headed: merged.headed,
      backend: merged.backend,
      browser: merged.browser,
      reporter: merged.reporter,
      strict: args.strict || undefined,
      order: args.order || undefined,
      language: args.language || undefined,
      video: merged.video || undefined,
      trace: merged.trace || undefined,
      storageState: merged.storageState || undefined,
      watch: merged.watch || undefined,
    };

    const runner = BddRunner.create(bddConfig);

    const { _setRunner } = await import('./bdd.js');
    _setRunner(runner);

    if (stepFiles.length > 0) {
      console.log(`  Loading ${stepFiles.length} step file(s)...`);
      for (const file of stepFiles) {
        await import(file);
      }
    } else {
      console.log('  No step definition files found. Using built-in steps only.');
    }

    console.log(`  Running features: ${featurePatterns.join(', ')}`);
    if (args.tags) console.log(`  Tags: ${args.tags}`);
    console.log();

    const summary = await runner.run();
    process.exit(summary.failed > 0 ? 1 : 0);
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
      '  ferridriver-test test                         # Run all E2E tests\n' +
      '  ferridriver-test test --headed -j 4           # 4 workers, headed browser\n' +
      '  ferridriver-test ct --framework react         # Component tests with React\n' +
      '  ferridriver-test bdd --tags "@smoke"           # BDD tests filtered by tag\n' +
      '  ferridriver-test bdd features/ --steps steps/  # Custom feature/step paths\n' +
      '  ferridriver-test codegen https://example.com   # Record interactions as test code\n' +
      '  ferridriver-test install --with-deps           # Install Chromium + system deps',
  },
  subCommands: {
    test: testCommand,
    ct: ctCommand,
    bdd: bddCommand,
    codegen: codegenCommand,
    install: installCommand,
  },
});

await runMain(withCompletions(root));
