// Shared helpers for the NAPI bun-test suite.
//
// `TestRunner.create()` consumes a serialised `FerridriverConfig`; tests build
// their own partial `TestConfig` here. The shape mirrors the generated
// TypeScript types in `packages/ferridriver-test/src/config-types`.

import { tmpdir } from 'os';
import { join } from 'path';
import { TestRunner, type NapiCliOverrides } from '../index.js';

/** Partial `TestConfig` plus a relaxed `reporter` (accepts string[] for brevity). */
export type TestOpts = Record<string, unknown> & {
  reporter?: string[] | Array<{ name: string; options?: Record<string, unknown> }>;
};

/** Normalize the reporter shorthand `['json']` into Rust's `[{ name: 'json' }]`. */
function normaliseReporter(rep: TestOpts['reporter']) {
  if (!rep) return undefined;
  if (rep.length === 0) return [];
  return rep.map((r) => (typeof r === 'string' ? { name: r, options: {} } : { name: r.name, options: r.options ?? {} }));
}

/** Build a JSON `FerridriverConfig` payload around the given test-section
 *  overrides. The default test config is single-worker + json reporter +
 *  isolated output dir so bun-test runs stay deterministic and clean. */
export function buildConfigJson(opts: TestOpts = {}): string {
  const { reporter, browser, ...rest } = opts;
  // Default the browser to headless so CI runners (no DISPLAY) launch
  // Chromium successfully. Tests can pass `browser: { headless: false }`
  // when they need headed behaviour.
  const browserDef = browser && typeof browser === 'object' ? (browser as Record<string, unknown>) : {};
  const test = {
    workers: 1,
    reporter: normaliseReporter(reporter) ?? [{ name: 'json', options: {} }],
    outputDir: join(tmpdir(), `ferri-${process.pid}-${Date.now()}-${Math.floor(Math.random() * 1e6)}`),
    screenshotOnFailure: false,
    browser: { headless: true, ...browserDef },
    ...rest,
  };
  return JSON.stringify({ test });
}

/** Convenience: create a `TestRunner` ready for `registerTestsBatch + run`.
 *  Pass `overrides` to layer CLI-only fields (e.g. `projectFilter`,
 *  `teardown`) -- they go through `runner.applyOverrides`, mirroring the
 *  production CLI flow. */
export function createRunner(opts: TestOpts = {}, overrides?: NapiCliOverrides): TestRunner {
  const runner = TestRunner.create(buildConfigJson(opts));
  if (overrides) runner.applyOverrides(overrides);
  return runner;
}
