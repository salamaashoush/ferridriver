#!/usr/bin/env bun
/**
 * Bundle the injected script into a single minified IIFE for browser injection.
 * Output: dist/engine.min.js
 *
 * `inlineCssPlugin` resolves Playwright-style `'./foo.css?inline'`
 * imports as default-exported strings of the file's contents (matches
 * Vite's `?inline` semantics). This lets us keep Playwright's injected/*
 * sources verbatim — `highlight.ts` has `import css from
 * './highlight.css?inline'` and we don't have to rewrite that to a
 * separate `highlightCss.ts` shim.
 */

import { mkdirSync, readFileSync } from 'fs';
import { resolve as resolvePath, dirname } from 'path';
import type { BunPlugin } from 'bun';

const inlineCssPlugin: BunPlugin = {
  name: 'inline-css',
  setup(builder) {
    builder.onResolve({ filter: /\.css\?inline$/ }, args => {
      const cleanPath = args.path.replace(/\?inline$/, '');
      const absolute = cleanPath.startsWith('.')
        ? resolvePath(dirname(args.importer), cleanPath)
        : cleanPath;
      return { path: absolute, namespace: 'inline-css' };
    });
    builder.onLoad({ filter: /\.css$/, namespace: 'inline-css' }, args => {
      const css = readFileSync(args.path, 'utf8');
      return { contents: `export default ${JSON.stringify(css)};`, loader: 'js' };
    });
  },
};

mkdirSync('./dist', { recursive: true });

const result = await Bun.build({
  entrypoints: ['./index.ts'],
  target: 'browser',
  minify: true,
  format: 'iife',
  sourcemap: 'none',
  plugins: [inlineCssPlugin],
});

if (!result.success) {
  console.error('Build failed:');
  for (const log of result.logs) {
    console.error(log);
  }
  process.exit(1);
}

const output = await result.outputs[0].text();
// Wrap in idempotency check
const wrapped = `(function(){if(window.__fd)return;${output}})()`;
await Bun.write('./dist/engine.min.js', wrapped);

const size = new Blob([wrapped]).size;
console.log(`Built dist/engine.min.js (${(size / 1024).toFixed(1)} KB)`);
