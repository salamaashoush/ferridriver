/**
 * defineConfig() — Playwright-compatible configuration helper.
 *
 * Usage:
 *   import { defineConfig } from '@ferridriver/test';
 *   export default defineConfig({
 *     workers: 4,
 *     use: { browserName: 'chromium', headless: true },
 *     projects: [
 *       { name: 'chromium', use: { browserName: 'chromium' } },
 *       { name: 'firefox', use: { browserName: 'firefox' } },
 *     ],
 *   });
 */

import type { TestRunnerConfig } from '@ferridriver/core';

/** Context / fixture options — matches Playwright's `use` block. */
export interface UseOptions {
  browserName?: 'chromium' | 'firefox' | 'webkit';
  headless?: boolean;
  channel?: string;
  viewport?: { width: number; height: number } | null;
  locale?: string;
  timezoneId?: string;
  geolocation?: { latitude: number; longitude: number; accuracy?: number };
  permissions?: string[];
  colorScheme?: 'light' | 'dark' | 'no-preference' | null;
  isMobile?: boolean;
  hasTouch?: boolean;
  javaScriptEnabled?: boolean;
  bypassCSP?: boolean;
  offline?: boolean;
  acceptDownloads?: boolean;
  userAgent?: string;
  extraHTTPHeaders?: Record<string, string>;
  httpCredentials?: { username: string; password: string; origin?: string };
  ignoreHTTPSErrors?: boolean;
  proxy?: { server: string; bypass?: string; username?: string; password?: string };
  storageState?: string | { cookies: any[]; origins: any[] };
  baseURL?: string;
  deviceScaleFactor?: number;
  reducedMotion?: 'reduce' | 'no-preference' | null;
  forcedColors?: 'active' | 'none' | null;
  serviceWorkers?: 'allow' | 'block';
  actionTimeout?: number;
  navigationTimeout?: number;
  testIdAttribute?: string;
  launchOptions?: Record<string, any>;
  connectOptions?: { wsEndpoint: string; headers?: Record<string, string>; timeout?: number };
}

/** Project configuration — matches Playwright's TestProject. */
export interface ProjectConfig {
  name: string;
  use?: UseOptions;
  testDir?: string;
  testMatch?: string | string[];
  testIgnore?: string | string[];
  outputDir?: string;
  snapshotDir?: string;
  snapshotPathTemplate?: string;
  timeout?: number;
  retries?: number;
  repeatEach?: number;
  fullyParallel?: boolean;
  grep?: RegExp | string;
  grepInvert?: RegExp | string;
  dependencies?: string[];
  teardown?: string;
  metadata?: Record<string, any>;
  tag?: string | string[];
}

/** Web server configuration — matches Playwright's webServer. */
export interface WebServerConfig {
  command?: string;
  url?: string;
  port?: number;
  timeout?: number;
  reuseExistingServer?: boolean;
  cwd?: string;
  env?: Record<string, string>;
  stdout?: 'pipe' | 'ignore' | 'inherit';
  stderr?: 'pipe' | 'ignore' | 'inherit';
  // Static file serving (ferridriver extension).
  staticDir?: string;
  spa?: boolean;
}

/** Reporter configuration. */
export type ReporterConfig =
  | string
  | [string, Record<string, any>];

/** Expect configuration. */
export interface ExpectConfig {
  timeout?: number;
  toHaveScreenshot?: {
    maxDiffPixels?: number;
    maxDiffPixelRatio?: number;
    threshold?: number;
    animations?: 'allow' | 'disabled';
  };
  toMatchSnapshot?: {
    maxDiffPixels?: number;
    maxDiffPixelRatio?: number;
    threshold?: number;
  };
}

/** Full test configuration — matches Playwright's PlaywrightTestConfig. */
export interface FerridriverTestConfig {
  // ── Test discovery ──
  testDir?: string;
  testMatch?: string | string[];
  testIgnore?: string | string[];

  // ── Execution ──
  timeout?: number;
  workers?: number | string;
  retries?: number;
  repeatEach?: number;
  fullyParallel?: boolean;
  forbidOnly?: boolean;
  maxFailures?: number;
  globalTimeout?: number;

  // ── Filtering ──
  grep?: RegExp | string;
  grepInvert?: RegExp | string;
  tag?: string | string[];

  // ── Output ──
  outputDir?: string;
  snapshotDir?: string;
  snapshotPathTemplate?: string;
  preserveOutput?: 'always' | 'never' | 'failures-only';
  quiet?: boolean;
  updateSnapshots?: 'all' | 'changed' | 'missing' | 'none';

  // ── Reporter ──
  reporter?: ReporterConfig | ReporterConfig[];
  reportSlowTests?: null | { max: number; threshold: number };

  // ── Browser / fixture options ──
  use?: UseOptions;

  // ── Projects ──
  projects?: ProjectConfig[];

  // ── Lifecycle ──
  globalSetup?: string | string[];
  globalTeardown?: string | string[];

  // ── Web server ──
  webServer?: WebServerConfig | WebServerConfig[];

  // ── Expect ──
  expect?: ExpectConfig;

  // ── Sharding ──
  shard?: { total: number; current: number };

  // ── Metadata ──
  metadata?: Record<string, any>;
  name?: string;
}

/**
 * defineConfig — type-safe configuration builder.
 *
 * Supports single config or merging multiple configs (like Playwright).
 */
export function defineConfig(config: FerridriverTestConfig): FerridriverTestConfig;
export function defineConfig(config: FerridriverTestConfig, ...overrides: FerridriverTestConfig[]): FerridriverTestConfig;
export function defineConfig(config: FerridriverTestConfig, ...overrides: FerridriverTestConfig[]): FerridriverTestConfig {
  if (overrides.length === 0) return config;
  // Shallow merge — later configs override earlier ones. Projects are concatenated.
  let merged = { ...config };
  for (const override of overrides) {
    const { projects: overrideProjects, ...rest } = override;
    merged = { ...merged, ...rest };
    if (overrideProjects) {
      merged.projects = [...(merged.projects ?? []), ...overrideProjects];
    }
  }
  return merged;
}
