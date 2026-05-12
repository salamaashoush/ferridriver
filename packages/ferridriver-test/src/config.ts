/**
 * defineConfig() — type-safe configuration helper.
 *
 * The full schema is generated from the Rust source of truth in
 * `crates/ferridriver-config` via ts-rs; see `./config-types/`. Users
 * typically supply a partial config and rely on defaults filled in by the
 * Rust runner.
 *
 * Usage:
 *   import { defineConfig } from '@ferridriver/test/config';
 *   export default defineConfig({
 *     workers: 4,
 *     browser: { browser: 'chromium', headless: true },
 *     projects: [
 *       { name: 'chromium', browser: { browser: 'chromium', backend: 'cdp-pipe' } },
 *     ],
 *   });
 */

export type * from './config-types/index.js';

import type { TestConfig, ProjectConfig, BrowserConfig, ContextConfig, FerridriverConfig } from './config-types/index.js';

/** Recursively mark every field as optional. User configs fill only what they need. */
export type DeepPartial<T> = T extends (...args: any[]) => any
  ? T
  : T extends ReadonlyArray<infer U>
    ? ReadonlyArray<DeepPartial<U>>
    : T extends Array<infer U>
      ? Array<DeepPartial<U>>
      : T extends object
        ? { [K in keyof T]?: DeepPartial<T[K]> }
        : T;

/** Function-form lifecycle hooks. `TestConfig.globalSetup` /
 *  `TestConfig.globalTeardown` accept file paths (which serialise into the
 *  unified config JSON); function-form callbacks ride alongside on these
 *  out-of-band fields and are registered with the runner via
 *  `runner.registerGlobalSetup(fn)` rather than the JSON pipe. */
export interface UserHooks {
  globalSetupFn?: () => void | Promise<void>;
  globalTeardownFn?: () => void | Promise<void>;
}

/** User-supplied test runner config: partial of the generated `TestConfig`
 *  plus the function-form hooks that can't be serialised. */
export type UserTestConfig = DeepPartial<TestConfig> & UserHooks;

/** User-supplied unified config: partial of the root `FerridriverConfig`. */
export type UserFerridriverConfig = DeepPartial<FerridriverConfig>;

/**
 * Backwards-compatible alias. Older imports referenced `FerridriverTestConfig`;
 * the canonical name is `UserTestConfig`. Both resolve to the same partial type.
 */
export type FerridriverTestConfig = UserTestConfig;

/**
 * defineConfig — type-safe builder for the `[test]` section.
 *
 * Supports merging multiple configs (later overrides earlier; `projects` arrays
 * are concatenated).
 */
export function defineConfig(config: UserTestConfig): UserTestConfig;
export function defineConfig(config: UserTestConfig, ...overrides: UserTestConfig[]): UserTestConfig;
export function defineConfig(config: UserTestConfig, ...overrides: UserTestConfig[]): UserTestConfig {
  if (overrides.length === 0) return config;
  let merged: UserTestConfig = { ...config };
  for (const override of overrides) {
    const { projects: overrideProjects, ...rest } = override;
    merged = { ...merged, ...rest };
    if (overrideProjects) {
      merged.projects = [...(merged.projects ?? []), ...overrideProjects] as ProjectConfig[];
    }
  }
  return merged;
}

/** Same as `defineConfig` but accepts the unified `[mcp] + [test]` shape. */
export function defineFerridriver(config: UserFerridriverConfig): UserFerridriverConfig {
  return config;
}

// Re-export common nested type aliases for ergonomic imports.
export type { BrowserConfig, ContextConfig, ProjectConfig, TestConfig, FerridriverConfig };
