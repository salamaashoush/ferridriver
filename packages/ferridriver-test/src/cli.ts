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

import { TestRunner } from '@ferridriver/node';
import type { Page, NapiCliOverrides } from '@ferridriver/node';
import { _setCurrentFile, _runWithFile, _drainTests, _hasOnly, _setCtMountFactory, _setRunner, _drainWorkerFixtures } from './test.js';
import type { MountFunction } from './test.js';
import { isAbsolute, join, relative, resolve } from 'path';
import { existsSync, statSync } from 'fs';

import type { UserTestConfig } from './config.js';

// ---- Profiling timers (enabled with --profile or FERRIDRIVER_PROFILE=cli) ----

const _profiling = process.argv.includes('--profile') || process.env.FERRIDRIVER_PROFILE === 'cli';
const _marks: { label: string; ms: number }[] = [];
let _phaseStart = performance.now();

function _markPhase(label: string) {
  if (!_profiling) return;
  const elapsed = performance.now() - _phaseStart;
  _marks.push({ label, ms: elapsed });
  _phaseStart = performance.now();
}

function _printProfile() {
  if (!_profiling || _marks.length === 0) return;
  const total = _marks.reduce((s, m) => s + m.ms, 0);
  const bar = (ms: number) => {
    const pct = (ms / total) * 100;
    const width = Math.max(1, Math.round(pct / 2));
    return '\u2588'.repeat(width);
  };
  console.log('\n  PROFILE: CLI phase breakdown');
  console.log('  \u2500'.repeat(60));
  for (const m of _marks) {
    const pct = ((m.ms / total) * 100).toFixed(1);
    console.log(`  ${m.label.padEnd(28)} ${m.ms.toFixed(1).padStart(8)}ms  ${pct.padStart(5)}%  ${bar(m.ms)}`);
  }
  console.log('  \u2500'.repeat(60));
  console.log(`  ${'Total'.padEnd(28)} ${total.toFixed(1).padStart(8)}ms`);
  console.log();
}

// ---- TS file loader: jiti on Node, native on Bun ----

let _importTs: (path: string) => Promise<any>;
let _configureTsLoader: (tsconfigPath: string | undefined) => void;

if (typeof globalThis.Bun !== 'undefined') {
  _importTs = (path: string) => import(path);
  // Bun honours the project's `tsconfig.json` automatically; the runtime
  // does not yet expose a programmatic override. When the user passes a
  // different tsconfig path we surface a one-time warning so the loader
  // behaviour is predictable rather than silently ignored.
  let _warned = false;
  _configureTsLoader = (tsconfigPath) => {
    if (tsconfigPath && !_warned) {
      _warned = true;
      console.warn(
        `[ferridriver-test] --tsconfig=${tsconfigPath} is honoured only when running under Node (jiti). Bun reads its own tsconfig.json.`,
      );
    }
  };
} else {
  const { createJiti } = await import('jiti');
  let _jiti = createJiti(import.meta.url, {
    fsCache: true,        // cache transpiled files to node_modules/.cache/jiti
    moduleCache: true,    // cache loaded modules in memory
    interopDefault: true, // auto-extract default exports
  });
  _importTs = (path: string) => _jiti.import(path);
  _configureTsLoader = (tsconfigPath) => {
    if (!tsconfigPath) return;
    _jiti = createJiti(import.meta.url, {
      fsCache: true,
      moduleCache: true,
      interopDefault: true,
      jsx: { factory: 'React.createElement', fragment: 'React.Fragment' },
      transformOptions: { ts: { compilerOptions: { tsconfig: tsconfigPath } } },
    } as any);
  };
}

// ---- Config file loading ----

const CONFIG_CANDIDATES = [
  'ferridriver.config.ts',
  'ferridriver.config.js',
  'ferridriver.config.mjs',
  'ferridriver.config.mts',
];

async function loadConfig(explicitPath?: string): Promise<UserTestConfig> {
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

/** Map CLI flags to the typed `NapiCliOverrides` consumed by the Rust runner.
 *
 * The file-loaded config (a partial `TestConfig`) is passed verbatim to
 * `TestRunner.create()` as JSON; CLI flags layer on top via
 * `runner.applyOverrides(buildOverrides(args))`. Runtime-only flags
 * (`grep`, `last-failed`, `watch`, `verbose`, `debug`) use dedicated
 * setters because they don't belong to the serialised schema. */
function buildOverrides(args: Record<string, any>): NapiCliOverrides {
  const o: NapiCliOverrides = {};
  if (args.workers !== undefined) o.workers = args.workers;
  if (args.retries !== undefined) o.retries = args.retries;
  if (args.timeout !== undefined) o.timeout = args.timeout;
  if (args.headed) o.headless = false;
  if (args['grep-invert']) o.grepInvert = args['grep-invert'];
  if (args.shard) {
    const m = String(args.shard).match(/^(\d+)\/(\d+)$/);
    if (m) {
      o.shardCurrent = Number(m[1]);
      o.shardTotal = Number(m[2]);
    }
  }
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
  if (args['update-snapshots']) o.updateSnapshots = args['update-snapshots'];
  if (args['forbid-only']) o.forbidOnly = true;
  if (args['max-failures'] !== undefined) o.maxFailures = args['max-failures'];
  if (args['repeat-each'] !== undefined) o.repeatEach = args['repeat-each'];
  if (args.x) o.failFast = true;
  if (args['pass-with-no-tests']) o.passWithNoTests = true;
  if (args['ignore-snapshots']) o.ignoreSnapshots = true;
  if (args.tsconfig) o.tsconfig = args.tsconfig;
  if (args['global-timeout'] !== undefined) o.globalTimeout = args['global-timeout'];
  if (args.name) o.name = args.name;
  if (args['fully-parallel']) o.fullyParallel = true;
  if (args.project) {
    o.projectFilter = Array.isArray(args.project) ? args.project : [args.project];
  }
  if (args['no-deps']) o.noDeps = true;
  if (args.teardown) o.teardown = args.teardown;
  if (args['only-changed'] !== undefined) o.onlyChanged = args['only-changed'];
  if (args['fail-on-flaky-tests']) o.failOnFlakyTests = true;
  if (args.video) o.video = args.video;
  if (args.trace) o.trace = args.trace;
  if (args['storage-state']) o.storageState = args['storage-state'];
  return o;
}

/** Apply runtime-only flags via dedicated NAPI setters (these aren't part of
 *  the serialised config schema). */
function applyRuntimeFlags(runner: TestRunner, args: Record<string, any>) {
  if (args.grep) runner.setGrep(args.grep);
  if (args['last-failed']) runner.setLastFailed(true);
  if (args.watch) runner.setWatch(true);
  if (args.verbose) {
    runner.setVerbose(1);
    if (!args.debug) runner.setDebug('*');
  }
  if (args.debug) runner.setDebug(args.debug);
}

/** Read a resolved value from the file config plus a CLI override, with CLI
 *  winning. Used by cli.ts for its own (non-runner) bookkeeping. */
function effective<T>(fileValue: T | undefined, cliValue: T | undefined): T | undefined {
  return cliValue !== undefined && cliValue !== false && cliValue !== null ? cliValue : fileValue;
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
    type: 'string',
    short: 'u',
    valueName: 'MODE',
    description: 'Update snapshot files. Optional MODE: all|changed|missing|none (default: changed)',
    valueParser: ['all', 'changed', 'missing', 'none'],
    defaultMissingValue: 'changed',
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
  // ── Cluster 1 surface ──
  'max-failures': {
    type: 'number',
    valueName: 'N',
    description: 'Stop after N failures (0 = unlimited)',
  },
  'repeat-each': {
    type: 'number',
    valueName: 'N',
    description: 'Run each test N times for flake detection',
  },
  x: {
    type: 'boolean',
    description: 'Stop after the first failure (alias of --max-failures 1)',
  },
  'pass-with-no-tests': {
    type: 'boolean',
    description: 'Make the run succeed even if no tests were discovered',
  },
  'ignore-snapshots': {
    type: 'boolean',
    description: 'Skip every snapshot comparison at runtime',
  },
  tsconfig: {
    type: 'string',
    valueName: 'PATH',
    description: 'Path to a single tsconfig used by the TS loader',
    valueHint: 'filePath',
  },
  'global-timeout': {
    type: 'number',
    valueName: 'MS',
    description: 'Maximum total runtime in ms across the whole test run',
  },
  name: {
    type: 'string',
    valueName: 'NAME',
    description: 'Display name for the run, surfaced in reports',
  },
  'fully-parallel': {
    type: 'boolean',
    description: 'Run all tests in parallel regardless of file-level grouping',
  },
  // ── Cluster 7 surface ──
  project: {
    type: 'string',
    action: 'append',
    valueName: 'NAME',
    description: 'Run only the named project(s) — repeatable',
  },
  'no-deps': {
    type: 'boolean',
    description: 'Skip project dependencies when filtering with --project',
  },
  teardown: {
    type: 'string',
    valueName: 'NAME',
    description: 'Run NAME as the run-wide teardown stage',
  },
  'only-changed': {
    type: 'string',
    valueName: 'REF',
    description: 'Only run test files changed between HEAD and REF (or working-tree if REF omitted)',
    defaultMissingValue: '',
  },
  'fail-on-flaky-tests': {
    type: 'boolean',
    description: 'Exit non-zero when any test passes only after a retry',
  },
  'capture-git-info': {
    type: 'boolean',
    description: 'Annotate the run summary with git commit / branch / dirty status',
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

function asStringArray(value?: string | string[]): string[] {
  if (!value) return [];
  return Array.isArray(value) ? value : [value];
}

function asCliList(value?: string | string[]): string[] {
  return asStringArray(value).filter((item) => item.length > 0);
}

function hasGlobMagic(value: string): boolean {
  return /[*?[\]{}()!+@]/.test(value);
}

function toGlobPath(value: string): string {
  return value.replace(/\\/g, '/');
}

function resolvePattern(pattern: string, baseDir?: string): string {
  if (isAbsolute(pattern)) return toGlobPath(pattern);
  if (!baseDir || baseDir === '.') return toGlobPath(pattern);
  return toGlobPath(join(baseDir, pattern));
}

function entryScopedPattern(pattern: string): string {
  const normalized = toGlobPath(pattern);
  const wildcardIndex = normalized.search(/[*?[\]{}()!+@]/);
  if (wildcardIndex >= 0) {
    const slashIndex = normalized.lastIndexOf('/', wildcardIndex);
    return slashIndex >= 0 ? normalized.slice(slashIndex + 1) : normalized;
  }
  const slashIndex = normalized.lastIndexOf('/');
  return slashIndex >= 0 ? normalized.slice(slashIndex + 1) : normalized;
}

function buildDirectoryPatterns(dir: string, matchPatterns: string[]): string[] {
  return matchPatterns.map((pattern) => resolvePattern(entryScopedPattern(pattern), dir));
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

async function collectPatterns(patterns: string[], cwd: string): Promise<string[]> {
  const results = await Promise.all(patterns.map((pattern) => collectGlob(pattern, cwd)));
  return results.flat();
}

type DiscoveryOptions = {
  entries: string[];
  matchPatterns: string[];
  ignorePatterns?: string[];
  baseDir?: string;
};

async function discoverFiles({ entries, matchPatterns, ignorePatterns = [], baseDir }: DiscoveryOptions): Promise<string[]> {
  const cwd = process.cwd();
  const scopedPatterns = matchPatterns.map((pattern) => resolvePattern(pattern, baseDir));
  const matchedFiles = new Set<string>();
  const unresolvedEntries: string[] = [];

  if (entries.length > 0) {
    for (const entry of entries) {
      const absoluteEntry = resolve(entry);
      try {
        const stats = statSync(absoluteEntry);
        if (stats.isDirectory()) {
          const filesInDirectory = await collectPatterns(buildDirectoryPatterns(entry, matchPatterns), cwd);
          filesInDirectory.forEach((file) => matchedFiles.add(file));
          continue;
        }
        if (stats.isFile()) {
          matchedFiles.add(absoluteEntry);
          continue;
        }
      } catch {
        // fall through to glob handling
      }

      if (hasGlobMagic(entry)) {
        const globMatches = await collectGlob(resolvePattern(entry, baseDir), cwd);
        if (globMatches.length > 0) {
          globMatches.forEach((file) => matchedFiles.add(file));
          continue;
        }
      }

      unresolvedEntries.push(entry);
    }
  } else {
    const discovered = await collectPatterns(scopedPatterns, cwd);
    discovered.forEach((file) => matchedFiles.add(file));
  }

  if (unresolvedEntries.length > 0) {
    const details = unresolvedEntries.map((entry) => `  - ${entry}`).join('\n');
    throw new Error(`No test files matched the provided path(s):\n${details}`);
  }

  const ignored = new Set<string>();
  if (ignorePatterns.length > 0) {
    const scopedIgnores = ignorePatterns.map((pattern) => resolvePattern(pattern, baseDir));
    const ignoredFiles = await collectPatterns(scopedIgnores, cwd);
    ignoredFiles.forEach((file) => ignored.add(file));
  }

  return [...matchedFiles]
    .filter((file) => !ignored.has(file))
    .sort();
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
  return [...new Set(results.flat())].sort();
}

// ---- E2E test runner (shared by default and ct modes) ----

async function runTests(
  fileConfig: UserTestConfig,
  args: Record<string, any>,
  testFiles: string[],
  ctMode: boolean,
  featureFiles: string[] = [],
  stepFiles: string[] = [],
) {
  _phaseStart = performance.now(); // reset so config+discovery captures time since CLI start
  let viteProcess: any = null;

  // Graceful shutdown on SIGINT/SIGTERM -- kill child processes and exit.
  const onSignal = () => {
    if (viteProcess) {
      viteProcess.stdout?.destroy();
      viteProcess.stderr?.destroy();
      viteProcess.kill();
    }
    process.exit(130);
  };
  process.on('SIGINT', onSignal);
  process.on('SIGTERM', onSignal);

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
          // Remove listeners once URL is found to prevent leaks and stale references.
          viteProcess.stdout?.off('data', onData);
          viteProcess.stderr?.off('data', onData);
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

    fileConfig.baseUrl = viteUrl;
    if (!fileConfig.workers) fileConfig.workers = 4;
    setupCtMount();
    console.log(`[ct] Serving at ${viteUrl}`);
  }

  // Normalize webServer to array for NAPI / config schema.
  if (fileConfig.webServer && !Array.isArray(fileConfig.webServer)) {
    fileConfig.webServer = [fileConfig.webServer as any];
  }

  // Re-create the TS loader with the user's tsconfig before any test files are imported.
  const tsconfig = effective<string>(fileConfig.tsconfig, args.tsconfig);
  _configureTsLoader(tsconfig);

  _markPhase('config + discovery');

  // Function-form hooks (`globalSetupFn` / `globalTeardownFn`) can't ride in
  // the serialised config payload, so strip them out before JSON.stringify
  // and register the callbacks on the runner separately.
  const { globalSetupFn, globalTeardownFn, ...serialisableConfig } = fileConfig as
    & UserTestConfig
    & { globalSetupFn?: () => void | Promise<void>; globalTeardownFn?: () => void | Promise<void> };

  const runner = TestRunner.create(JSON.stringify({ test: serialisableConfig }));
  runner.applyOverrides(buildOverrides(args));
  applyRuntimeFlags(runner, args);
  if (typeof globalSetupFn === 'function') {
    runner.registerGlobalSetup(async () => { await globalSetupFn(); });
  }
  if (typeof globalTeardownFn === 'function') {
    runner.registerGlobalTeardown(async () => { await globalTeardownFn(); });
  }
  _setRunner(runner);
  _markPhase('TestRunner.create (NAPI)');

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
  _markPhase(`import ${testFiles.length} test files`);

  const tests = _drainTests();
  if (tests.length > 0) {
    // Batch-register all tests in a single NAPI call: one lock acquisition,
    // one boundary crossing, pre-allocated capacity.
    runner.registerTestsBatch(tests.map(t => ({ meta: t.meta, callback: t.body })));
  }
  _markPhase(`register ${tests.length} tests`);

  if (tests.length === 0 && featureFiles.length === 0) {
    console.log('  No tests found.');
    _printProfile();
    if (viteProcess) {
      viteProcess.stdout?.destroy();
      viteProcess.stderr?.destroy();
      viteProcess.kill();
    }
    // Playwright `--pass-with-no-tests`: exit 0 when no tests are discovered;
    // otherwise the empty run is treated as a failure (exit 1).
    process.exit(effective(fileConfig.passWithNoTests, args['pass-with-no-tests']) ? 0 : 1);
  }

  // Run — feature files passed to Rust for parsing/translation into the same plan.
  const summary = await runner.run(featureFiles.length > 0 ? featureFiles : undefined);
  _markPhase('runner.run (execution)');

  // Drain worker-scoped fixture teardowns (unblocks factory cleanup code).
  await _drainWorkerFixtures();

  if (viteProcess) {
    viteProcess.stdout?.destroy();
    viteProcess.stderr?.destroy();
    viteProcess.kill();
  }
  // Force exit — NAPI native addon may hold browser process handles that prevent
  // clean shutdown. process.exit() is the correct behavior here (same as Playwright).
  _printProfile();
  if (summary.exitCode !== 0 && summary.total === 0) {
    console.error('  Test run failed before executing any tests.');
  }
  // FERRIDRIVER_RTT_STATS dump — Bun / some Node configurations exit
  // without firing libc atexit (per-dispatcher Drops also leak via
  // tokio reader/writer tasks), so explicit dump avoids losing the
  // table. No-op when the env var is unset.
  try {
    const { dumpRttStats } = await import('@ferridriver/node');
    if (typeof dumpRttStats === 'function') dumpRttStats();
  } catch {
    /* native addon may not yet expose the helper on older builds */
  }
  const exitCode = summary.exitCode !== 0 ? summary.exitCode : (summary.failed > 0 ? 1 : 0);
  process.exit(exitCode);
}

// ---- Commands ----

const testCommand = defineCommand({
  meta: {
    name: 'test',
    description: 'Run tests (.spec.ts, .test.ts, .feature, or mixed)',
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

    // BDD config rides on the unified config schema (test.tags / strict / order
    // / language). CLI flags overwrite whatever the file declared so the
    // resolved JSON shipped to the runner already reflects --tags etc.
    if (args.tags) fileConfig.tags = args.tags;
    if (args.strict) fileConfig.strict = true;
    if (args.order) fileConfig.order = args.order;
    if (args.language) fileConfig.language = args.language;

    // Discover all files — use testMatch from config, or default patterns.
    const fileList = asCliList(args.files as string[] | string | undefined);
    const defaultPatterns = ['**/*.spec.ts', '**/*.test.ts', '**/*.feature'];
    const patterns = asStringArray(fileConfig.testMatch).length > 0
      ? asStringArray(fileConfig.testMatch)
      : defaultPatterns;
    const ignorePatterns = asStringArray(fileConfig.testIgnore);
    const allFiles = await discoverFiles({
      entries: fileList,
      matchPatterns: patterns,
      ignorePatterns,
      baseDir: fileConfig.testDir,
    });

    let testFiles = allFiles.filter(f => /\.(spec|test)\.[tj]sx?$/.test(f));
    let featureFiles = allFiles.filter(f => f.endsWith('.feature'));

    // `--only-changed [ref]`: intersect discovered files with the
    // git diff. Empty value falls back to the working-tree diff.
    // Outside a git repo we keep the original file set and emit a warning.
    const onlyChanged = args['only-changed'];
    if (typeof onlyChanged === 'string') {
      const { spawnSync } = await import('child_process');
      const ref = onlyChanged;
      const gitArgs = ref ? ['diff', '--name-only', ref, 'HEAD'] : ['status', '--porcelain'];
      const proc = spawnSync('git', gitArgs, { encoding: 'utf8' });
      if (proc.status === 0) {
        const lines = proc.stdout.split('\n').map((l: string) => l.trim()).filter(Boolean);
        const changed = new Set<string>();
        for (const line of lines) {
          // git status --porcelain prefixes each entry with `XY ` (3 chars).
          const path = ref ? line : line.length > 3 ? line.slice(3).trim() : '';
          if (!path) continue;
          changed.add(resolve(path));
        }
        const matches = (file: string) => changed.has(resolve(file));
        testFiles = testFiles.filter(matches);
        featureFiles = featureFiles.filter(matches);
      } else {
        console.warn('[ferridriver-test] --only-changed: git unavailable or not a repo, ignoring filter');
      }
    }

    if (testFiles.length === 0 && featureFiles.length === 0) {
      console.log('  No test files found.');
      process.exit(effective(fileConfig.passWithNoTests, args['pass-with-no-tests']) ? 0 : 1);
    }

    // Load step definitions if we have feature files.
    const stepsGlobs = asCliList(args.steps as string[] | string | undefined);
    const stepFiles = featureFiles.length > 0 ? await discoverStepFiles(stepsGlobs) : [];

    await runTests(fileConfig, args, testFiles, false, featureFiles, stepFiles);
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
    const fileList = asCliList(args.files as string[] | string | undefined);
    const testFiles = await discoverFiles({
      entries: fileList,
      matchPatterns: ['**/*.ct.ts', '**/*.ct.tsx', '**/*.ct.spec.ts', '**/*.ct.spec.tsx'],
      ignorePatterns: asStringArray(fileConfig.testIgnore),
      baseDir: fileConfig.testDir,
    });
    if (testFiles.length === 0) {
      console.log('  No component test files found.');
      process.exit(effective(fileConfig.passWithNoTests, args['pass-with-no-tests']) ? 0 : 1);
    }
    await runTests(fileConfig, args, testFiles, true);
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
    const { Codegen } = await import('@ferridriver/node');

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

const mergeReportsCommand = defineCommand({
  meta: {
    name: 'merge-reports',
    description: 'Merge blob reports (one per shard) into a unified report',
  },
  args: {
    dir: { type: 'positional', valueName: 'DIR', required: true, description: 'Directory containing blob *.zip files' },
    reporter: {
      type: 'string',
      description: 'Reporter to drive with the merged event stream (comma-separated for multiple)',
      default: 'terminal',
    },
    output: {
      type: 'string',
      valueName: 'DIR',
      description: 'Output directory for merged-reporter artefacts (default: ./merged-report)',
      valueHint: 'filePath',
    },
  },
  async run({ args }) {
    const { mergeReports } = await import('@ferridriver/node');
    const reporters = String(args.reporter).split(',').map((s) => s.trim()).filter(Boolean);
    const summary = await mergeReports(args.dir as string, reporters, args.output as string | undefined);
    console.log(
      `merged: ${summary.total} total, ${summary.passed} passed, ${summary.failed} failed, ${summary.skipped} skipped, ${summary.flaky} flaky`,
    );
    process.exit(summary.exitCode);
  },
});

const installCommand = defineCommand({
  meta: {
    name: 'install',
    description: 'Install browsers for automation',
  },
  args: {
    'with-deps': {
      type: 'boolean' as const,
      description: 'Also install system dependencies (fonts, libs)',
      default: false,
    },
    browser: {
      type: 'positional' as const,
      valueName: 'BROWSER',
      description: 'Browser to install (default: chromium)',
    },
  },
  async run({ args }) {
    const { installChromium, installChromiumHeadlessShell, installSystemDeps, getBrowserCacheDir } = await import('@ferridriver/node');
    const browser = (args.browser as string) || 'chromium';
    if (!['chromium', 'chrome', 'chromium-headless-shell', 'firefox'].includes(browser)) {
      console.error(`Unsupported browser: ${browser}. Supported: chromium, chromium-headless-shell, firefox.`);
      process.exit(1);
    }
    console.log(`Browser cache: ${getBrowserCacheDir()}`);
    if (args['with-deps']) {
      console.log('Installing system dependencies...');
      await installSystemDeps();
      console.log('System dependencies installed.');
    }
    if (browser === 'chromium-headless-shell') {
      console.log('Installing Chrome Headless Shell...');
      const path = await installChromiumHeadlessShell();
      console.log(`Chrome Headless Shell installed: ${path}`);
    } else {
      console.log('Installing Chromium...');
      const path = await installChromium();
      console.log(`Chromium installed: ${path}`);
    }
  },
});

const root = defineCommand({
  meta: {
    name: 'ferridriver-test',
    version: '0.1.0',
    description: 'E2E, component, and BDD test runner — Playwright-compatible, Rust-powered',
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
    'merge-reports': mergeReportsCommand,
  },
});

await runMain(withCompletions(root));
