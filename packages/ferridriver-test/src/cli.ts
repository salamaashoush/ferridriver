#!/usr/bin/env bun
/**
 * ferridriver-test CLI — runs E2E tests using the Rust engine.
 *
 * Usage:
 *   ferridriver-test [files...] [--workers N] [--retries N] [--headed] [--grep pattern]
 */

import { TestRunner } from 'ferridriver';
import { _setCurrentFile, _drainTests, _hasOnly } from './test.js';
import { resolve, relative } from 'path';
import { Glob } from 'bun';

// ── Parse CLI args ──

const args = process.argv.slice(2);
let files: string[] = [];
const config: Record<string, any> = {};

for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === '--workers' || arg === '-j') config.workers = parseInt(args[++i]);
  else if (arg === '--retries') config.retries = parseInt(args[++i]);
  else if (arg === '--timeout') config.timeout = parseInt(args[++i]);
  else if (arg === '--headed') config.headed = true;
  else if (arg === '--grep' || arg === '-g') config.grep = args[++i];
  else if (arg === '--backend') config.backend = args[++i];
  else if (arg === '--reporter') config.reporter = [args[++i]];
  else if (!arg.startsWith('-')) files.push(arg);
}

// ── Discover test files ──

async function discoverFiles(): Promise<string[]> {
  if (files.length > 0) {
    return files.map((f) => resolve(f));
  }

  // Default: **/*.spec.ts, **/*.test.ts
  const patterns = ['**/*.spec.ts', '**/*.test.ts'];
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

// ── Main ──

async function main() {
  const testFiles = await discoverFiles();

  if (testFiles.length === 0) {
    console.log('  No test files found.');
    process.exit(0);
  }

  const runner = await TestRunner.create(config);
  const workerCount = runner.workerCount();

  // Load each test file to collect test() registrations.
  for (const file of testFiles) {
    _setCurrentFile(relative(process.cwd(), file));
    await import(file);
  }

  const tests = _drainTests();

  if (tests.length === 0) {
    console.log('  No tests found.');
    process.exit(0);
  }

  // If test.only() was used, filter to only those.
  const filtered = _hasOnly()
    ? tests.filter((t) => t.meta.modifier === 'only')
    : tests;

  // Apply grep filter.
  const grepped = config.grep
    ? filtered.filter((t) => new RegExp(config.grep).test(t.meta.title))
    : filtered;

  // Register with Rust runner.
  for (const t of grepped) {
    runner.registerTest(t.meta, t.body);
  }

  console.log(`\n  Running ${grepped.length} test(s) with ${workerCount} worker(s)\n`);

  // Run — Rust handles everything: browsers, dispatch, retries, timeouts.
  const summary = await runner.run();

  // Print results.
  for (const r of summary.results) {
    const icon = r.status === 'passed' ? '✓' :
                 r.status === 'failed' || r.status === 'timed out' ? '✗' :
                 r.status === 'skipped' ? '−' :
                 r.status === 'flaky' ? '⚠' : '?';
    const color = r.status === 'passed' ? '\x1b[32m' :
                  r.status === 'failed' || r.status === 'timed out' ? '\x1b[31m' :
                  r.status === 'skipped' ? '\x1b[33m' :
                  r.status === 'flaky' ? '\x1b[33m' : '';
    const reset = '\x1b[0m';
    const duration = r.status !== 'skipped' ? ` (${Math.round(r.durationMs)}ms)` : '';
    console.log(`  ${color}${icon}${reset} ${r.title}${duration}`);
    if (r.errorMessage) {
      console.log(`    ${'\x1b[31m'}${r.errorMessage}${'\x1b[0m'}\n`);
    }
  }

  // Summary line.
  const parts: string[] = [];
  if (summary.passed > 0) parts.push(`\x1b[32m${summary.passed} passed\x1b[0m`);
  if (summary.failed > 0) parts.push(`\x1b[31m${summary.failed} failed\x1b[0m`);
  if (summary.flaky > 0) parts.push(`\x1b[33m${summary.flaky} flaky\x1b[0m`);
  if (summary.skipped > 0) parts.push(`\x1b[33m${summary.skipped} skipped\x1b[0m`);
  console.log(`\n  ${summary.total} test(s): ${parts.join(', ')} (${Math.round(summary.durationMs)}ms)\n`);

  process.exit(summary.failed > 0 ? 1 : 0);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
