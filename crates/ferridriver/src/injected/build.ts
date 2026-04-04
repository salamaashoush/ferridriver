#!/usr/bin/env bun
/**
 * Bundle the injected script into a single minified IIFE for browser injection.
 * Output: dist/engine.min.js
 */

import { mkdirSync } from 'fs';

mkdirSync('./dist', { recursive: true });

const result = await Bun.build({
  entrypoints: ['./index.ts'],
  target: 'browser',
  minify: true,
  format: 'iife',
  sourcemap: 'none',
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
