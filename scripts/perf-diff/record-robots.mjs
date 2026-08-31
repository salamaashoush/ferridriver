// Record what `robots-parser` answers, so the Rust port can be checked
// against the package rather than against someone's reading of the
// specification.
//
//   node record-robots.mjs            # check the recording is current
//   node record-robots.mjs --update   # re-record
//
// `is-crawlable` does not parse robots.txt itself: Lighthouse imports
// `robots-parser`, so agreeing with Lighthouse means agreeing with this
// package, down to the details no specification pins -- what an empty
// `Disallow:` does to the `*` fallback, which of two equal-length rules
// wins, how a pattern is percent-encoded before it is matched.
//
// The cases are here and the answers are in the recording, so
// `cargo test -p ferridriver-perf --test robots` needs neither node nor
// the package. Add a case whenever the port grows a branch: the
// recording is the only thing saying the branch is right.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

import robotsParser from 'robots-parser';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '../..');
const RECORDING = join(ROOT, 'crates/ferridriver-perf/tests/fixtures/robots.json');

// Lighthouse pins `^3.0.1`. A different resolution is a different set
// of answers, and the recording would not say which produced it.
const EXPECTED_VERSION = '3.0.1';

// Each file is served from `http://example.com/robots.txt` unless the
// case says otherwise.
const FILES = {
  empty: '',
  blockAll: 'User-agent: *\nDisallow: /',
  allowAll: 'User-agent: *\nDisallow:',
  // Longest match wins, and Allow breaks a tie at equal length.
  precedence: 'User-agent: *\nDisallow: /a/\nAllow: /a/b/\nDisallow: /page\nAllow: /page',
  wildcards: 'User-agent: *\nDisallow: /*.pdf$\nDisallow: /private*\nDisallow: /*?session=',
  // A named group replaces the wildcard group rather than adding to it.
  perBot:
    'User-agent: *\nDisallow: /\n\nUser-agent: Googlebot\nAllow: /\n\n' +
    'User-agent: bingbot\nDisallow:\n\nUser-agent: slowbot\nCrawl-delay: 10',
  // Consecutive User-agent lines share one group; a non-User-agent line
  // between them starts a new one.
  groups: 'User-agent: a\nUser-agent: b\nDisallow: /x\n\nUser-agent: c\nDisallow: /y',
  comments: 'User-agent: * # everyone\nDisallow: /x # not this\n# nothing at all\n\nAllow: /x/ok',
  // Percent-encoding is normalised on both sides before matching.
  encoded: 'User-agent: *\nDisallow: /a%2fb\nDisallow: /café\nDisallow: /a b\nDisallow: /100%25',
  // Lines that carry no colon, or an empty field, are skipped without
  // ending the current group -- which is what keeps line numbers right.
  ragged: 'User-agent: *\nthis line has no colon\n: empty field\nDisallow: /x',
  crlf: 'User-agent: *\r\nDisallow: /x\r\nAllow: /x/ok',
  // Sitemap and Host are parsed and neither contributes a rule.
  directives: 'Sitemap: http://example.com/sitemap.xml\nHost: example.com\nUser-agent: *\nDisallow: /x',
};

const AGENTS = [undefined, 'Googlebot', 'googlebot/2.1', 'bingbot', 'slowbot', 'a', 'c', 'DuckDuckBot'];

/** One (file, robots.txt url, page url) triple; every agent is asked about each. */
const CASES = [
  { file: 'empty', url: 'http://example.com/anything' },
  { file: 'blockAll', url: 'http://example.com/' },
  { file: 'blockAll', url: 'http://example.com/deep/path' },
  // Another origin, another scheme and another port: this file governs
  // none of them, and "does not govern" is not "allowed".
  { file: 'blockAll', url: 'http://elsewhere.com/' },
  { file: 'blockAll', url: 'https://example.com/' },
  { file: 'blockAll', url: 'http://example.com:8080/' },
  // The default port is the same origin as no port at all.
  { file: 'blockAll', at: 'http://example.com:80/robots.txt', url: 'http://example.com/' },
  { file: 'blockAll', at: 'https://example.com:443/robots.txt', url: 'https://example.com/' },
  { file: 'allowAll', url: 'http://example.com/x' },
  { file: 'precedence', url: 'http://example.com/a/c' },
  { file: 'precedence', url: 'http://example.com/a/b/c' },
  { file: 'precedence', url: 'http://example.com/page' },
  { file: 'wildcards', url: 'http://example.com/a/b/c.pdf' },
  { file: 'wildcards', url: 'http://example.com/a/b/c.pdf.html' },
  { file: 'wildcards', url: 'http://example.com/private/thing' },
  { file: 'wildcards', url: 'http://example.com/x?session=1' },
  { file: 'wildcards', url: 'http://example.com/x?other=1' },
  { file: 'wildcards', url: 'http://example.com/x?' },
  { file: 'wildcards', url: 'http://example.com/x#session=1' },
  { file: 'perBot', url: 'http://example.com/x' },
  { file: 'groups', url: 'http://example.com/x' },
  { file: 'groups', url: 'http://example.com/y' },
  { file: 'comments', url: 'http://example.com/x' },
  { file: 'comments', url: 'http://example.com/x/ok' },
  { file: 'encoded', url: 'http://example.com/a%2Fb' },
  { file: 'encoded', url: 'http://example.com/a%2fb' },
  { file: 'encoded', url: 'http://example.com/café' },
  { file: 'encoded', url: 'http://example.com/a%20b' },
  { file: 'encoded', url: 'http://example.com/100%25' },
  { file: 'ragged', url: 'http://example.com/x' },
  { file: 'crlf', url: 'http://example.com/x' },
  { file: 'crlf', url: 'http://example.com/x/ok' },
  { file: 'directives', url: 'http://example.com/x' },
  // A relative page URL, which the package resolves against a domain of
  // its own -- so it belongs to no real robots.txt.
  { file: 'blockAll', url: '/x' },
];

function record() {
  const results = [];
  for (const { file, at = 'http://example.com/robots.txt', url } of CASES) {
    const robots = robotsParser(at, FILES[file]);
    for (const agent of AGENTS) {
      results.push({
        file,
        at,
        url,
        agent: agent ?? null,
        allowed: robots.isAllowed(url, agent) ?? null,
        line: robots.getMatchingLineNumber(url, agent),
      });
    }
  }
  return `${JSON.stringify({ parser: EXPECTED_VERSION, files: FILES, cases: results }, null, 1)}\n`;
}

const installed = JSON.parse(
  readFileSync(join(HERE, 'node_modules/robots-parser/package.json'), 'utf8'),
).version;
if (installed !== EXPECTED_VERSION) {
  console.error(
    `robots-parser ${installed} is installed, this records ${EXPECTED_VERSION}.\n` +
      'Lighthouse pins ^3.0.1; change EXPECTED_VERSION here only alongside a Lighthouse bump.',
  );
  process.exit(1);
}

const fresh = record();
const where = relative(ROOT, RECORDING);
if (process.argv.includes('--update')) {
  writeFileSync(RECORDING, fresh);
  console.log(`recorded ${where}`);
} else {
  let current = null;
  try {
    current = readFileSync(RECORDING, 'utf8');
  } catch {}
  if (current !== fresh) {
    console.error(`${where}: out of date with robots-parser ${installed}\n\nrun: just robots-diff --update`);
    process.exit(1);
  }
  console.log(`${JSON.parse(fresh).cases.length} answers still match robots-parser ${installed}`);
}
