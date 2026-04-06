#!/usr/bin/env bun
/**
 * ferridriver-test CLI — runs E2E, component, and BDD tests using the Rust engine.
 *
 * E2E mode (default):
 *   ferridriver-test [files...] [--workers N] [--retries N] [--headed]
 *
 * Component testing mode:
 *   ferridriver-test --ct [files...] [--framework react]
 *
 * BDD mode:
 *   ferridriver-test --bdd [features...] [--steps steps/] [--tags "@smoke"]
 *
 * The --bdd flag activates the BDD pipeline:
 *   1. Discovers step definition files (--steps glob)
 *   2. Imports them to register Given/When/Then via NAPI
 *   3. Discovers .feature files
 *   4. Rust parses Gherkin, matches steps, executes via core TestRunner
 *
 * The --ct flag activates the CT pipeline:
 *   1. Scans test files for component imports (import transform)
 *   2. Starts Vite dev server with component registry
 *   3. Runs tests with mount() fixture provided to each test
 *   4. Shuts down Vite on completion
 */

import { TestRunner, BddRunner } from 'ferridriver';
import type { Page, BddRunnerConfig } from 'ferridriver';
import { _setCurrentFile, _drainTests, _hasOnly, _setCtMountFactory } from './test.js';
import type { MountFunction } from './test.js';
import { resolve, relative } from 'path';
import { Glob } from 'bun';

// ── Parse CLI args ──

const args = process.argv.slice(2);
let files: string[] = [];
const config: Record<string, any> = {};
let ctMode = false;
let bddMode = false;
let ctFramework: string | null = null;
let ctRegisterSource: string | null = null;
let bddStepsGlobs: string[] = [];
let bddTags: string | null = null;
let bddFeatureGlobs: string[] = [];
let bddDryRun = false;
let bddList = false;

for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === '--workers' || arg === '-j') config.workers = parseInt(args[++i]);
  else if (arg === '--retries') config.retries = parseInt(args[++i]);
  else if (arg === '--timeout') config.timeout = parseInt(args[++i]);
  else if (arg === '--headed') config.headed = true;
  else if (arg === '--grep' || arg === '-g') config.grep = args[++i];
  else if (arg === '--backend') config.backend = args[++i];
  else if (arg === '--reporter') config.reporter = [args[++i]];
  else if (arg === '--ct') ctMode = true;
  else if (arg === '--bdd') bddMode = true;
  else if (arg === '--steps') bddStepsGlobs.push(args[++i]);
  else if (arg === '--tags' || arg === '-t') bddTags = args[++i];
  else if (arg === '--features') bddFeatureGlobs.push(args[++i]);
  else if (arg === '--dry-run') bddDryRun = true;
  else if (arg === '--list') bddList = true;
  else if (arg === '--framework') ctFramework = args[++i];
  else if (arg === '--register-source') ctRegisterSource = args[++i];
  else if (arg === '--update-snapshots') config.updateSnapshots = true;
  else if (!arg.startsWith('-')) files.push(arg);
}

// ── Discover test files ──

async function discoverFiles(): Promise<string[]> {
  if (files.length > 0) return files.map((f) => resolve(f));

  const patterns = ctMode
    ? ['**/*.ct.ts', '**/*.ct.tsx', '**/*.ct.spec.ts', '**/*.ct.spec.tsx']
    : ['**/*.spec.ts', '**/*.test.ts'];

  const found: string[] = [];
  for (const pattern of patterns) {
    const glob = new Glob(pattern);
    for await (const file of glob.scan({ cwd: process.cwd(), absolute: true })) {
      if (!file.includes('node_modules')) found.push(file);
    }
  }
  found.sort();
  return found;
}

// ── CT: resolve framework adapter ──

async function resolveCtAdapter(): Promise<{
  registerSourcePath: string;
  frameworkPlugin: (() => Promise<any>) | null;
}> {
  if (ctRegisterSource) {
    return { registerSourcePath: resolve(ctRegisterSource), frameworkPlugin: null };
  }

  const fw = ctFramework || 'react';
  const pkg = `@ferridriver/ct-${fw}`;

  // Resolve from the user's project directory (cwd), not from the CLI's location.
  // This ensures bun workspace links in the project's node_modules are found.
  const { createRequire } = await import('module');
  const projectRequire = createRequire(resolve('package.json'));

  // Try: 1) resolve from project, 2) bare import, 3) monorepo fallback.
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
    `Or specify: --register-source path/to/registerSource.mjs`
  );
}

// ── CT: wire mount() into test fixtures ──

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

// ── BDD mode ──

async function discoverStepFiles(): Promise<string[]> {
  const patterns = bddStepsGlobs.length > 0
    ? bddStepsGlobs
    : ['steps/**/*.ts', 'steps/**/*.js', 'step_definitions/**/*.ts', 'step_definitions/**/*.js'];

  const found: string[] = [];
  for (const pattern of patterns) {
    const glob = new Glob(pattern);
    for await (const file of glob.scan({ cwd: process.cwd(), absolute: true })) {
      if (!file.includes('node_modules')) found.push(file);
    }
  }
  found.sort();
  return found;
}

async function runBdd() {
  // Determine feature patterns.
  const featurePatterns = bddFeatureGlobs.length > 0
    ? bddFeatureGlobs
    : files.length > 0
      ? files
      : ['features/**/*.feature'];

  // Discover and import step definition files.
  const stepFiles = await discoverStepFiles();

  // Create BDD runner with config.
  const bddConfig: BddRunnerConfig = {
    features: featurePatterns,
    tags: bddTags || undefined,
    workers: config.workers,
    timeout: config.timeout,
    retries: config.retries,
    headed: config.headed,
    backend: config.backend,
    reporter: config.reporter,
    outputDir: config.outputDir,
  };

  const runner = BddRunner.create(bddConfig);

  // Set the runner so Given/When/Then in step files register into it.
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

  // Run features.
  console.log(`  Running features: ${featurePatterns.join(', ')}`);
  if (bddTags) console.log(`  Tags: ${bddTags}`);
  console.log();

  const summary = await runner.run();

  // Exit.
  process.exit(summary.failed > 0 ? 1 : 0);
}

// ── Main ──

async function main() {
  // ── BDD mode ──
  if (bddMode) {
    await runBdd();
    return;
  }

  const testFiles = await discoverFiles();

  if (testFiles.length === 0) {
    console.log('  No test files found.');
    process.exit(0);
  }

  // ── CT mode: start Vite dev server for the project ──
  let viteProcess: any = null;
  if (ctMode) {
    // Start the project's own Vite dev server.
    // This serves the full app (index.html + all sources).
    // Tests can interact with the app directly OR use mount() for isolated components.
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

    // Wait for Vite to print URL.
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

    // Pre-warm: fetch the page to trigger Vite's on-demand compilation.
    // This compiles React/Vue/Svelte/Solid BEFORE any browser connects,
    // so browsers get pre-compiled JS instantly.
    const warmStart = Date.now();
    for (let i = 0; i < 60; i++) {
      try {
        const resp = await fetch(viteUrl);
        if (resp.ok) {
          // Fetch the JS entry too to trigger full compilation.
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
    // Cap workers for CT — Chrome launch + navigate dominates.
    // 4 workers is the sweet spot (benchmarked).
    if (!config.workers) config.workers = 4;
    setupCtMount();
    console.log(`[ct] Serving at ${viteUrl}`);
  }

  // ── Create Rust runner ──
  const runner = await TestRunner.create(config);
  const workerCount = runner.workerCount();

  // ── Load test files ──
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

  const filtered = _hasOnly() ? tests.filter((t) => t.meta.modifier === 'only') : tests;
  const grepped = config.grep
    ? filtered.filter((t) => new RegExp(config.grep).test(t.meta.title))
    : filtered;

  for (const t of grepped) runner.registerTest(t.meta, t.body);

  const mode = ctMode ? 'component' : 'E2E';
  console.log(`\n  Running ${grepped.length} ${mode} test(s) with ${workerCount} worker(s)\n`);

  const summary = await runner.run();

  for (const r of summary.results) {
    const icon = r.status === 'passed' ? '✓' : r.status === 'failed' || r.status === 'timed out' ? '✗' :
                 r.status === 'skipped' ? '−' : r.status === 'flaky' ? '⚠' : '?';
    const color = r.status === 'passed' ? '\x1b[32m' : r.status === 'failed' || r.status === 'timed out' ? '\x1b[31m' :
                  '\x1b[33m';
    const dur = r.status !== 'skipped' ? ` (${Math.round(r.durationMs)}ms)` : '';
    console.log(`  ${color}${icon}\x1b[0m ${r.title}${dur}`);
    if (r.errorMessage) console.log(`    \x1b[31m${r.errorMessage}\x1b[0m\n`);
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

main().catch((e) => { console.error(e); process.exit(1); });
