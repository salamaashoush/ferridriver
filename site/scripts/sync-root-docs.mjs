#!/usr/bin/env node
/**
 * Mirror repo-root CHANGELOG.md and CONTRIBUTING.md into site/docs.
 * Rewrites repo-relative links (./README.md, ./site/, ./docs/) to absolute
 * GitHub URLs so the site build does not flag them as dead links.
 */

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const siteRoot = resolve(__dirname, '..');
const repoRoot = resolve(siteRoot, '..');

const GH = 'https://github.com/salamaashoush/ferridriver/blob/main';

/** Rewrite known relative links to absolute GitHub URLs. */
function rewriteRepoLinks(md) {
  return md
    // ./path → GitHub link, preserving anchors
    .replace(/\]\(\.\/([^)]+)\)/g, (_, p) => {
      // Keep site-internal absolute links untouched
      return `](${GH}/${p})`;
    });
}

const files = [
  { src: 'CHANGELOG.md',     dest: 'docs/changelog.md',     title: 'Changelog' },
  { src: 'CONTRIBUTING.md',  dest: 'docs/contributing.md',  title: 'Contributing' },
];

for (const { src, dest } of files) {
  const raw = readFileSync(resolve(repoRoot, src), 'utf8');
  const rewritten = rewriteRepoLinks(raw);
  writeFileSync(resolve(siteRoot, dest), rewritten);
  console.log(`synced ${src} → ${dest}`);
}
