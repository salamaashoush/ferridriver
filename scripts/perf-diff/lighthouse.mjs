// Run Lighthouse's own audits against a live page and print what they
// concluded, so an audit ported into ferridriver can be checked against
// the one it was modelled on.
//
//   node lighthouse.mjs <url> [chrome-executable] [--navigation]
//
// Snapshot mode by default: it reads the DOM as it stands and navigates
// nothing, so a static fixture gives the same answer every run. That
// makes the comparison a gate rather than a timing race.
//
// `--navigation` drives a real load instead, which is the only way to
// reach the audits that need a network log: `canonical`,
// `http-status-code`, `is-on-https`, `is-crawlable`, `has-hsts`,
// `csp-xss`. Their verdicts are still deterministic against a static
// fixture, because each is a function of the response headers, the
// status and the DOM rather than of timing — the performance numbers
// in the same run are NOT, which is why nothing here compares them.
//
// Chrome is not bundled. Pass the executable, or set CHROME_PATH.
import puppeteer from 'puppeteer-core';
import { navigation, snapshot } from 'chrome-devtools-mcp/build/src/third_party/lighthouse-devtools-mcp-bundle.js';

const args = process.argv.slice(2);
const navigate = args.includes('--navigation');
const [url, chromeArg] = args.filter(a => a !== '--navigation');
const executablePath = chromeArg ?? process.env.CHROME_PATH;
if (!url || !executablePath) {
  console.error('usage: node lighthouse.mjs <url> [chrome-executable] [--navigation]   (or set CHROME_PATH)');
  process.exit(2);
}

const browser = await puppeteer.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
try {
  const page = await browser.newPage();
  const { lhr } = navigate
    ? await navigation(page, url, { flags: { output: 'json' } })
    : await (async () => {
        await page.goto(url, { waitUntil: 'networkidle0' });
        return snapshot(page, { flags: { output: 'json' } });
      })();

  // `notApplicable` means the page had nothing for the audit to look
  // at, which is not a verdict and nothing to compare against.
  const audits = {};
  for (const [id, audit] of Object.entries(lhr.audits)) {
    if (audit.scoreDisplayMode === 'notApplicable') continue;
    const record = { score: audit.score, mode: audit.scoreDisplayMode };
    if (audit.numericValue !== undefined) record.numericValue = audit.numericValue;
    // The offending nodes, by the selector Lighthouse reports them
    // under. Counts alone hide which element an audit disagreed about.
    const items = audit.details?.items ?? [];
    if (items.length) {
      record.items = items
        .map(item => item.node?.selector ?? item.node?.snippet ?? item.source?.url ?? item.href ?? item.text)
        .filter(Boolean)
        .slice(0, 20);
      record.itemCount = items.length;
    }
    audits[id] = record;
  }
  // Which audit ids are axe rules, straight from the category rather
  // than from a list someone maintains here. Everything Lighthouse files
  // under accessibility is a wrapper that looks up a rule id in what axe
  // already decided, so this is the set the two engines can be compared
  // over: outside it an id like `meta-description` is Lighthouse's own
  // audit and means nothing to axe.
  const axeRules = (lhr.categories?.accessibility?.auditRefs ?? []).map(ref => ref.id).sort();

  console.log(JSON.stringify({ url, mode: navigate ? 'navigation' : 'snapshot', lighthouse: lhr.lighthouseVersion, axeRules, audits }, null, 1));
} finally {
  await browser.close();
}
