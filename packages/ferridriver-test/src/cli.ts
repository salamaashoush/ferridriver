#!/usr/bin/env bun
/**
 * ferridriver-test CLI — runs E2E and component tests using the Rust engine.
 *
 * E2E mode (default):
 *   ferridriver-test [files...] [--workers N] [--retries N] [--headed]
 *
 * Component testing mode:
 *   ferridriver-test --ct [files...] [--framework react]
 *
 * The --ct flag activates the CT pipeline:
 *   1. Scans test files for component imports (import transform)
 *   2. Starts Vite dev server with component registry
 *   3. Runs tests with mount() fixture provided to each test
 *   4. Shuts down Vite on completion
 */

import { TestRunner } from 'ferridriver';
import type { Page } from 'ferridriver';
import { _setCurrentFile, _drainTests, _hasOnly, _setCtMountFactory } from './test.js';
import type { MountFunction } from './test.js';
import { resolve, relative } from 'path';
import { Glob } from 'bun';

// ── Parse CLI args ──

const args = process.argv.slice(2);
let files: string[] = [];
const config: Record<string, any> = {};
let ctMode = false;
let ctFramework: string | null = null;
let ctRegisterSource: string | null = null;

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

  for (const path of [pkg, resolve(`packages/ct-${fw}/src/index.mjs`)]) {
    try {
      const adapter = await import(path);
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

// ── Main ──

async function main() {
  const testFiles = await discoverFiles();

  if (testFiles.length === 0) {
    console.log('  No test files found.');
    process.exit(0);
  }

  // ── CT mode: build + serve before loading tests ──
  let ctRunner: any = null;
  if (ctMode) {
    const adapter = await resolveCtAdapter();

    let createCtRunner: any;
    for (const path of ['@ferridriver/ct-core/runner', resolve('packages/ct-core/src/runner.mjs')]) {
      try { createCtRunner = (await import(path)).createCtRunner; break; } catch {}
    }
    if (!createCtRunner) throw new Error('Cannot find @ferridriver/ct-core.');

    ctRunner = await createCtRunner({
      projectDir: resolve('.'),
      testFiles,
      registerSourcePath: adapter.registerSourcePath,
      frameworkPlugin: adapter.frameworkPlugin,
    });

    config.base_url = ctRunner.baseUrl;
    setupCtMount();

    console.log(`[ct] ${ctRunner.registry.size} component(s), serving at ${ctRunner.baseUrl}`);
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
    if (ctRunner) await ctRunner.stop();
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

  if (ctRunner) await ctRunner.stop();
  process.exit(summary.failed > 0 ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });
