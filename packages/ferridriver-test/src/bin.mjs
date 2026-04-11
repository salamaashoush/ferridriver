#!/usr/bin/env node

/**
 * CLI entry point.
 *
 * On Bun: imports cli.ts directly (native TS support).
 * On Node.js: re-spawns with --experimental-transform-types and a resolve
 * hook that maps .js imports to .ts files.
 */

import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

if (typeof globalThis.Bun !== 'undefined') {
  await import('./cli.ts');
} else if (process.env.__FERRIDRIVER_LOADER === '1') {
  await import('./cli.ts');
} else {
  const registerPath = fileURLToPath(new URL('./register.mjs', import.meta.url));
  try {
    execFileSync(process.execPath, [
      '--experimental-transform-types',
      '--import', registerPath,
      ...process.argv.slice(1),
    ], {
      stdio: 'inherit',
      env: { ...process.env, __FERRIDRIVER_LOADER: '1' },
    });
  } catch (e) {
    process.exit(e.status ?? 1);
  }
}
