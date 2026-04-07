#!/usr/bin/env bun
/**
 * ferridriver-test CLI -- runs E2E, component, and BDD tests using the Rust engine.
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

import { TestRunner, BddRunner } from 'ferridriver';
import type { Page, BddRunnerConfig } from 'ferridriver';
import { _setCurrentFile, _drainTests, _hasOnly, _setCtMountFactory } from './test.js';
import type { MountFunction } from './test.js';
import { resolve, relative } from 'path';
import { statSync } from 'fs';

// ---- Glob abstraction: use Bun.Glob if available, fall back to node:fs glob ----

async function* scanGlob(pattern: string, cwd: string): AsyncIterable<string> {
  if (typeof globalThis.Bun !== 'undefined') {
    const { Glob } = await import('bun');
    const g = new Glob(pattern);
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
    description: 'Browser backend',
    valueParser: ['cdp-pipe', 'cdp-raw', 'webkit'],
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
  verbose: {
    type: 'boolean',
    short: 'v',
    description: 'Verbose output (debug-level logging)',
  },
  debug: {
    type: 'string',
    description: 'Debug categories: cdp, steps, action, worker, fixture (comma-separated)',
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

  const mode = ctMode ? 'component' : 'E2E';
  console.log(`\n  Running ${tests.length} ${mode} test(s) with ${workerCount} worker(s)\n`);

  const summary = await runner.run();

  for (const r of summary.results) {
    const icon = r.status === 'passed' ? '\u2713' : r.status === 'failed' || r.status === 'timed out' ? '\u2717' :
                 r.status === 'skipped' ? '\u2212' : r.status === 'flaky' ? '\u26a0' : '?';
    const color = r.status === 'passed' ? '\x1b[32m' : r.status === 'failed' || r.status === 'timed out' ? '\x1b[31m' :
                  '\x1b[33m';
    const dur = r.status !== 'skipped' ? ` (${Math.round(r.durationMs)}ms)` : '';
    console.log(`  ${color}${icon}\x1b[0m ${r.title}${dur}`);
    if (r.errorMessage) {
      // Strip NAPI error wrapper prefix (e.g. "GenericFailure, Error: ")
      const msg = r.errorMessage.replace(/^GenericFailure,\s*Error:\s*/, '');
      const indented = msg.split('\n').map((l: string) => `      ${l}`).join('\n');
      console.log(`\x1b[31m${indented}\x1b[0m\n`);
    }
  }

  const parts: string[] = [];
  if (summary.passed > 0) parts.push(`\x1b[32m${summary.passed} passed\x1b[0m`);
  if (summary.failed > 0) parts.push(`\x1b[31m${summary.failed} failed\x1b[0m`);
  if (summary.flaky > 0) parts.push(`\x1b[33m${summary.flaky} flaky\x1b[0m`);
  if (summary.skipped > 0) parts.push(`\x1b[33m${summary.skipped} skipped\x1b[0m`);
  console.log(`\n  ${summary.total} test(s): ${parts.join(', ')} (${Math.round(summary.durationMs)}ms)\n`);

  if (viteProcess) viteProcess.kill();
  process.exit(summary.failed > 0 ? 1 : 0);
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
    const fileList = (args.files as string[] | undefined) ?? [];
    const testFiles = await discoverFiles(fileList, ['**/*.spec.ts', '**/*.test.ts']);
    if (testFiles.length === 0) {
      console.log('  No test files found.');
      process.exit(0);
    }
    const config: Record<string, any> = {};
    if (args.workers !== undefined) config.workers = args.workers;
    if (args.retries !== undefined) config.retries = args.retries;
    if (args.timeout !== undefined) config.timeout = args.timeout;
    if (args.headed) config.headed = true;
    if (args.grep) config.grep = args.grep;
    if (args.backend) config.backend = args.backend;
    if (args.reporter) config.reporter = [args.reporter];
    if (args['update-snapshots']) config.updateSnapshots = true;
    if (args['forbid-only']) config.forbidOnly = true;
    if (args['last-failed']) config.lastFailed = true;
    if (args.video) config.video = args.video;
    if (args.trace) config.trace = args.trace;
    if (args['storage-state']) config.storageState = args['storage-state'];
    if (args.verbose) config.verbose = 1;
    if (args.debug) config.debug = args.debug;
    else if (args.verbose) config.debug = '*';
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
    const fileList = (args.files as string[] | undefined) ?? [];
    const testFiles = await discoverFiles(fileList, [
      '**/*.ct.ts', '**/*.ct.tsx', '**/*.ct.spec.ts', '**/*.ct.spec.tsx',
    ]);
    if (testFiles.length === 0) {
      console.log('  No component test files found.');
      process.exit(0);
    }
    const config: Record<string, any> = {};
    if (args.workers !== undefined) config.workers = args.workers;
    if (args.retries !== undefined) config.retries = args.retries;
    if (args.timeout !== undefined) config.timeout = args.timeout;
    if (args.headed) config.headed = true;
    if (args.grep) config.grep = args.grep;
    if (args.backend) config.backend = args.backend;
    if (args.reporter) config.reporter = [args.reporter];
    if (args['update-snapshots']) config.updateSnapshots = true;
    if (args['forbid-only']) config.forbidOnly = true;
    if (args['last-failed']) config.lastFailed = true;
    if (args.video) config.video = args.video;
    if (args.trace) config.trace = args.trace;
    if (args['storage-state']) config.storageState = args['storage-state'];
    if (args.verbose && !process.env.FERRIDRIVER_DEBUG) process.env.FERRIDRIVER_DEBUG = '*';
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
    const featureFiles = (args.features as string[] | undefined) ?? [];
    const featurePatterns = featureFiles.length > 0
      ? normalizePaths(featureFiles, '**/*.feature')
      : ['features/**/*.feature'];

    const stepsGlobs = (args.steps as string[] | undefined) ?? [];
    const stepFiles = await discoverStepFiles(stepsGlobs);

    const bddConfig: BddRunnerConfig = {
      features: featurePatterns,
      tags: args.tags || undefined,
      workers: args.workers,
      timeout: args.timeout ?? args['step-timeout'],
      retries: args.retries,
      headed: args.headed,
      backend: args.backend,
      reporter: args.reporter ? [args.reporter] : undefined,
      strict: args.strict || undefined,
      order: args.order || undefined,
      language: args.language || undefined,
      video: args.video || undefined,
      trace: args.trace || undefined,
      storageState: args['storage-state'] || undefined,
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
      '  ferridriver-test codegen https://example.com   # Record interactions as test code',
  },
  subCommands: {
    test: testCommand,
    ct: ctCommand,
    bdd: bddCommand,
    codegen: codegenCommand,
  },
});

void runMain(withCompletions(root));
