/**
 * ferridriver CT import transform.
 *
 * Scans test files for component imports and rewrites them to importRef
 * descriptors. Also collects the component registry for the Vite plugin.
 *
 * This is a simplified version of Playwright's tsxTransform.ts.
 * Uses regex-based parsing (good enough for typical imports).
 * Can be replaced with OXC (Rust-native AST transform) later.
 *
 * What it does:
 *
 * BEFORE:
 *   import Counter from './Counter';
 *   import { Button } from '../components/Button';
 *
 * AFTER:
 *   const Counter = { __pw_type: 'importRef', id: '_src_Counter' };
 *   const Button = { __pw_type: 'importRef', id: '_components_Button', property: 'Button' };
 *
 * And populates componentRegistry with:
 *   '_src_Counter' → { importSource: './Counter', remoteName: 'default' }
 *   '_components_Button' → { importSource: '../components/Button', remoteName: 'Button' }
 */

import { readFileSync } from "fs";
import { resolve, dirname, relative, join } from "path";

/**
 * Known non-component packages that should NOT be rewritten.
 * Imports from these are left as-is.
 */
const SKIP_PACKAGES = new Set([
  "react",
  "react-dom",
  "react-dom/client",
  "vue",
  "svelte",
  "solid-js",
  "solid-js/web",
  "@ferridriver/ct-react",
  "@ferridriver/ct-vue",
  "@ferridriver/ct-svelte",
  "@ferridriver/ct-core",
  "bun:test",
  "vitest",
  "node:*",
]);

function shouldSkip(source) {
  if (source.startsWith("node:")) return true;
  if (!source.startsWith(".") && !source.startsWith("/")) {
    // Bare specifier — skip if it's a known non-component package.
    const pkg = source.startsWith("@")
      ? source.split("/").slice(0, 2).join("/")
      : source.split("/")[0];
    return SKIP_PACKAGES.has(pkg) || SKIP_PACKAGES.has(source);
  }
  return false;
}

/**
 * Compute a deterministic component ID from a file path.
 * Replaces non-word chars with underscores.
 */
function makeId(importSource, testFile) {
  const resolved = resolve(dirname(testFile), importSource);
  // Use path relative to cwd, replace non-word chars.
  const rel = relative(process.cwd(), resolved);
  return rel.replace(/[^\w]/g, "_");
}

/**
 * Transform a test file: rewrite component imports to importRef descriptors.
 *
 * @param {string} code - source code of the test file
 * @param {string} filename - absolute path to the test file
 * @param {Map<string, object>} componentRegistry - mutated: id → { importSource, remoteName }
 * @returns {string} - transformed code
 */
export function transformTestFile(code, filename, componentRegistry) {
  // Match import statements.
  // Handles: import X from '...', import { X } from '...', import { X as Y } from '...'
  const importRegex =
    /^import\s+(?:(\w+)|{([^}]+)})\s+from\s+['"]([^'"]+)['"]\s*;?\s*$/gm;

  let result = code;
  const replacements = [];

  let match;
  while ((match = importRegex.exec(code)) !== null) {
    const [fullMatch, defaultImport, namedImports, source] = match;

    if (shouldSkip(source)) continue;

    // Only rewrite imports that look like component files.
    // Component files: .tsx, .jsx, .vue, .svelte, or extensionless (resolved by bundler).
    // Skip: .css, .json, .wasm, .mjs, .mts, .js (non-component), .ts (test utils)
    const isComponentImport =
      source.endsWith(".tsx") ||
      source.endsWith(".jsx") ||
      source.endsWith(".vue") ||
      source.endsWith(".svelte") ||
      // Extensionless relative imports are assumed to be components.
      (source.startsWith(".") && !source.includes("."));

    if (!isComponentImport) continue;

    const constLines = [];

    if (defaultImport) {
      const id = makeId(source, filename);
      componentRegistry.set(id, {
        id,
        importSource: source,
        remoteName: "default",
        filename,
      });
      constLines.push(
        `const ${defaultImport} = { __pw_type: 'importRef', id: '${id}' };`
      );
    }

    if (namedImports) {
      const names = namedImports.split(",").map((s) => s.trim());
      for (const name of names) {
        const [original, alias] = name.includes(" as ")
          ? name.split(" as ").map((s) => s.trim())
          : [name, name];
        const id = makeId(source, filename) + "_" + original;
        componentRegistry.set(id, {
          id,
          importSource: source,
          remoteName: original,
          filename,
        });
        constLines.push(
          `const ${alias} = { __pw_type: 'importRef', id: '${id}', property: '${original}' };`
        );
      }
    }

    if (constLines.length > 0) {
      replacements.push({
        start: match.index,
        end: match.index + fullMatch.length,
        replacement: constLines.join("\n"),
      });
    }
  }

  // Apply replacements in reverse order to preserve offsets.
  for (const { start, end, replacement } of replacements.reverse()) {
    result = result.slice(0, start) + replacement + result.slice(end);
  }

  return result;
}

/**
 * Scan multiple test files and collect the component registry.
 *
 * @param {string[]} testFiles - absolute paths to test files
 * @returns {{ registry: Map<string, object>, transformedFiles: Map<string, string> }}
 */
export function scanTestFiles(testFiles) {
  const registry = new Map();
  const transformedFiles = new Map();

  for (const file of testFiles) {
    const code = readFileSync(file, "utf-8");
    const transformed = transformTestFile(code, file, registry);
    transformedFiles.set(file, transformed);
  }

  return { registry, transformedFiles };
}
