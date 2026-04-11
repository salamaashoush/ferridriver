/**
 * Node.js ESM loader hook for TypeScript files.
 *
 * Registered via `module.register()` in the CLI entry point.
 * Intercepts .ts/.tsx/.mts imports and transpiles them to JS using
 * ferridriver's built-in oxc transpiler (via NAPI). No external
 * tooling (tsx, ts-node, esbuild) needed.
 *
 * In-memory cache ensures shared imports are only transpiled once.
 */

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

/** @type {Map<string, string>} file path → transpiled JS */
const cache = new Map();

/** @type {import('@ferridriver/core').transformTypeScript | null} */
let _transform = null;

async function getTransform() {
  if (!_transform) {
    const core = await import('@ferridriver/core');
    _transform = core.transformTypeScript;
  }
  return _transform;
}

const TS_RE = /\.[mc]?tsx?$/;

/**
 * ESM load hook — called for every module load.
 * Transpiles TypeScript files, passes everything else through.
 */
export async function load(url, context, nextLoad) {
  // Only handle TypeScript files outside node_modules.
  if (TS_RE.test(url) && !url.includes('node_modules')) {
    const filePath = fileURLToPath(url);

    let source = cache.get(filePath);
    if (!source) {
      const transform = await getTransform();
      const code = readFileSync(filePath, 'utf-8');
      source = transform(code, filePath);
      cache.set(filePath, source);
    }

    return { format: 'module', source, shortCircuit: true };
  }

  return nextLoad(url, context);
}
