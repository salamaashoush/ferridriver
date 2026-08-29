// Run Lighthouse's own audits against a live page and print what they
// concluded, so an audit ported into ferridriver can be checked against
// the one it was modelled on.
//
//   node lighthouse.mjs <url> [chrome-executable]
//
// Snapshot mode, deliberately: it reads the DOM as it stands and
// navigates nothing, so a static fixture gives the same answer every
// run. That makes the comparison a real gate rather than a timing race.
// The trade is that audits needing a network log (`is-on-https`,
// `csp-xss`, `has-hsts`, `canonical`, `is-crawlable`, ...) do not run
// here; those need navigation mode and a different kind of harness.
//
// Chrome is not bundled. Pass the executable, or set CHROME_PATH.
import puppeteer from 'puppeteer-core';
import { snapshot } from 'chrome-devtools-mcp/build/src/third_party/lighthouse-devtools-mcp-bundle.js';

const [url, chromeArg] = process.argv.slice(2);
const executablePath = chromeArg ?? process.env.CHROME_PATH;
if (!url || !executablePath) {
  console.error('usage: node lighthouse.mjs <url> [chrome-executable]   (or set CHROME_PATH)');
  process.exit(2);
}

const browser = await puppeteer.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
try {
  const page = await browser.newPage();
  await page.goto(url, { waitUntil: 'networkidle0' });
  const { lhr } = await snapshot(page, { flags: { output: 'json' } });

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
  console.log(JSON.stringify({ url, lighthouse: lhr.lighthouseVersion, audits }, null, 1));
} finally {
  await browser.close();
}
