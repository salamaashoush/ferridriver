// `page.checkPageQuality()` — the Lighthouse audits that score a page
// as it stands, ported in `ferridriver::audits`. Seven of them
// Lighthouse also scores in snapshot mode; `canonical`,
// `http-status-code` and `is-crawlable` it scores only after a
// navigation, which is why the recorded gate keeps a second recording
// per fixture page.
//
// What these agree with is checked elsewhere: `just quality-diff` compares
// the verdicts against Lighthouse's on the same pages and
// `cargo test -p ferridriver-perf --test lighthouse` gates it. Here is
// where they run on every backend, because the artifacts come out of one
// evaluate returning a JSON string and each backend serialises that
// differently — because `preventsPaste` is not a read but a
// synthetic ClipboardEvent, which is exactly the kind of thing that
// works on one engine and not another — and because the last two audits
// read the main document's response out of the network log and fetch
// `/robots.txt` from the page, neither of which any two backends
// deliver the same way.

import { test, describe, expect } from '@ferridriver/test';
import type { PageQualityReport } from '@ferridriver/test';

// setContent leaves the document at about:blank, where a relative href
// resolves to nothing and `crawlable-anchors` fails it — which is what
// Lighthouse concludes there too. Absolute hrefs keep these specs about
// the audits rather than about the document URL.
// `http-status-code` and `is-crawlable` are absent from this list on
// purpose: every spec below that uses setContent leaves the document at
// about:blank, which arrived with no response for them to read.
const DOM_AUDIT_IDS = [
  'doctype',
  'meta-description',
  'canonical',
  'crawlable-anchors',
  'link-text',
  'image-aspect-ratio',
  'image-size-responsive',
  'paste-preventing-inputs',
];
const AUDIT_IDS = [...DOM_AUDIT_IDS, 'http-status-code', 'is-crawlable'];

const failing = (report: PageQualityReport) => report.audits.filter((a) => !a.passed).map((a) => a.id);
const audit = (report: PageQualityReport, id: string) => report.audits.find((a) => a.id === id)!;

describe('page.checkPageQuality', () => {
  test('reports every DOM audit, with a category and a title on each', async ({ page }) => {
    await page.setContent(
      "<!doctype html><html lang='en'><head><title>t</title>" +
        "<meta name='description' content='A page with a description'></head>" +
        "<body><main><p><a href='https://example.com/pricing'>See our pricing</a></p></main></body></html>",
    );
    const report = await page.checkPageQuality();

    expect(report.audits.map((a) => a.id)).toEqual(DOM_AUDIT_IDS);
    expect(failing(report)).toEqual([]);
    expect(audit(report, 'canonical').category).toBe('seo');
    expect(audit(report, 'meta-description').category).toBe('seo');
    expect(audit(report, 'doctype').category).toBe('best-practices');
    expect(audit(report, 'doctype').title).toBe('Page has the HTML doctype');
  });

  test('a missing doctype and an empty description fail with their own explanations', async ({ page }) => {
    // No doctype puts the document in quirks mode, which is the state
    // this audit exists to catch.
    await page.setContent(
      "<html lang='en'><head><title>t</title><meta name='description' content='  '></head><body></body></html>",
    );
    const report = await page.checkPageQuality();

    expect(audit(report, 'doctype').passed).toBe(false);
    expect(audit(report, 'doctype').explanation).toBe('Document must contain a doctype');
    expect(audit(report, 'meta-description').passed).toBe(false);
    expect(audit(report, 'meta-description').explanation).toBe('Description text is empty.');
  });

  test('an uncrawlable link and an undescriptive one are reported with the element', async ({ page }) => {
    await page.setContent(
      "<!doctype html><html lang='en'><head><title>t</title>" +
        "<meta name='description' content='d'></head><body><main>" +
        "<p><a id='void' href='javascript:void(0)'>Open</a></p>" +
        "<p><a id='vague' href='https://example.com/pricing'>click here</a></p>" +
        '</main></body></html>',
    );
    const report = await page.checkPageQuality();

    const crawlable = audit(report, 'crawlable-anchors');
    expect(crawlable.passed).toBe(false);
    expect(crawlable.items.length).toBe(1);
    expect(crawlable.items[0].selector).toBe('#void');
    expect(crawlable.items[0].detail).toBe('javascript:void(0)');

    const text = audit(report, 'link-text');
    expect(text.passed).toBe(false);
    expect(text.items.length).toBe(1);
    expect(text.items[0].detail).toBe('click here');
  });

  test('a link to the page it sits on is judged on neither count', async ({ page }) => {
    await page.setContent(
      "<!doctype html><html lang='en'><head><title>t</title>" +
        "<meta name='description' content='d'></head>" +
        "<body><main><p><a href='#section'>here</a></p><h2 id='section'>Section</h2></main></body></html>",
    );
    const report = await page.checkPageQuality();
    expect(audit(report, 'link-text').passed).toBe(true);
    expect(audit(report, 'crawlable-anchors').passed).toBe(true);
  });

  test('an input that cancels paste is caught by dispatching one', async ({ page }) => {
    // Not an attribute read: the audit fires a cancelable ClipboardEvent
    // and sees whether anything cancelled it, so a listener added with
    // addEventListener counts just as much as the attribute.
    await page.setContent(
      "<!doctype html><html lang='en'><head><title>t</title>" +
        "<meta name='description' content='d'></head><body><main>" +
        "<input id='blocked' type='text'><input id='fine' type='text'>" +
        "<script>document.getElementById('blocked')" +
        ".addEventListener('paste', e => e.preventDefault())</script>" +
        '</main></body></html>',
    );
    const report = await page.checkPageQuality();

    const paste = audit(report, 'paste-preventing-inputs');
    expect(paste.passed).toBe(false);
    expect(paste.items.length).toBe(1);
    expect(paste.items[0].selector).toBe('#blocked');
    expect(paste.items[0].detail).toBe('text');
  });

  test('a relative canonical fails and an absolute one passes', async ({ page }) => {
    const withCanonical = (href: string) =>
      "<!doctype html><html lang='en'><head><title>t</title>" +
      "<meta name='description' content='d'>" +
      `<link rel='canonical' href='${href}'>` +
      '</head><body><main><p>x</p></main></body></html>';

    await page.setContent(withCanonical('https://example.com/page'));
    expect(audit(await page.checkPageQuality(), 'canonical').passed).toBe(true);

    // Relative: it resolves, but only against this document, so it
    // names no address a crawler arriving from elsewhere can use.
    await page.setContent(withCanonical('/page'));
    const relative = audit(await page.checkPageQuality(), 'canonical');
    expect(relative.passed).toBe(false);
    expect(relative.explanation).toContain('Relative');
  });

  test('only narrows the run', async ({ page }) => {
    await page.setContent('<html><body></body></html>');
    const report = await page.checkPageQuality({ only: ['doctype', 'meta-description'] });
    expect(report.audits.map((a) => a.id)).toEqual(['doctype', 'meta-description']);
  });

  // The two that read more than the DOM. They need a real navigation:
  // the status and the response headers come from the network log, and
  // `/robots.txt` is fetched from the page's own origin.

  test('a page that arrived over HTTP is scored on its status and its crawlability', async ({ page }) => {
    await page.goto('/fx/landed');
    const report = await page.checkPageQuality();

    expect(report.audits.map((a) => a.id)).toEqual(AUDIT_IDS);
    expect(audit(report, 'http-status-code').passed).toBe(true);
    expect(audit(report, 'is-crawlable').passed).toBe(true);
  });

  test('a document with no response of its own is not scored on either', async ({ page }) => {
    // Claiming a successful status for a page nothing was served for
    // would be an answer invented rather than observed.
    await page.setContent('<!doctype html><html lang="en"><body></body></html>');
    const report = await page.checkPageQuality();
    expect(report.audits.map((a) => a.id)).toEqual(DOM_AUDIT_IDS);
  });

  test('a 404 and a 500 fail the status audit and a 200 does not', async ({ page }) => {
    // Which statuses fall in the failing range is settled by the unit
    // tests; what this proves is that the status reaches the audit at
    // all, which it does through the network log rather than the DOM.
    for (const status of [404, 500]) {
      await page.goto(`/fx/status/${status}`);
      const failed = audit(await page.checkPageQuality(), 'http-status-code');
      expect(failed.passed).toBe(false);
      expect(failed.explanation).toBe(String(status));
    }

    await page.goto('/fx/status/200');
    expect(audit(await page.checkPageQuality(), 'http-status-code').passed).toBe(true);
  });

  test('a robots meta saying noindex blocks every crawler', async ({ page }) => {
    await page.goto('/fx/landed');
    await page.setContent(
      '<!doctype html><html lang="en"><head><title>t</title>' +
        "<meta name='description' content='d'><meta name='robots' content='noindex'></head>" +
        '<body><main><p>x</p></main></body></html>',
    );
    const report = audit(await page.checkPageQuality(), 'is-crawlable');
    expect(report.passed).toBe(false);
    expect(report.items.length).toBe(1);
    expect(report.items[0].detail).toBe('noindex');
  });

  test('a meta naming one bot leaves the others crawling', async ({ page }) => {
    // Only one meta is read per crawler, so googlebot naming itself
    // replaces the generic one rather than adding to it — and one bot
    // still able to index is enough to pass.
    await page.goto('/fx/landed');
    await page.setContent(
      '<!doctype html><html lang="en"><head><title>t</title>' +
        "<meta name='description' content='d'><meta name='robots' content='noindex'>" +
        "<meta name='googlebot' content='all'></head><body><main><p>x</p></main></body></html>",
    );
    expect(audit(await page.checkPageQuality(), 'is-crawlable').passed).toBe(true);
  });

  test('an X-Robots-Tag header blocks, and one naming a single bot does not', async ({ page }) => {
    await page.goto('/fx/robots-header?tag=noindex');
    const blocked = audit(await page.checkPageQuality(), 'is-crawlable');
    expect(blocked.passed).toBe(false);
    // The header reached us with its name and value intact, which is
    // the part the network log has to carry on every backend.
    expect(blocked.items.some((item) => item.detail === 'noindex')).toBe(true);

    await page.goto('/fx/robots-header?tag=Googlebot%3A%20noindex');
    expect(audit(await page.checkPageQuality(), 'is-crawlable').passed).toBe(true);
  });

  test('both copies of a header sent twice reach the audit', async ({ page }) => {
    // How many ITEMS this reports is engine-dependent and cannot be
    // made otherwise: Gecko joins repeated response headers before
    // WebDriver BiDi ever sees them, so Firefox reports the pair as one
    // header where Chromium and WebKit report two. What is the same
    // everywhere is that both directives were read, and the verdict.
    await page.goto('/fx/robots-header?tag=noindex&tag=none');
    const report = audit(await page.checkPageQuality(), 'is-crawlable');
    expect(report.passed).toBe(false);
    const directives = report.items.map((item) => item.detail).join(' ');
    expect(directives).toContain('noindex');
    expect(directives).toContain('none');
  });

  test('an unavailable_after date in the past blocks and one in the future does not', async ({ page }) => {
    // `unavailable_after:` reads exactly like a `googlebot:` prefix and
    // is not one; mistaking it for a prefix would skip it for every bot.
    await page.goto('/fx/robots-header?tag=unavailable_after%3A%2025%20Jun%202010%2015%3A00%3A00%20PST');
    expect(audit(await page.checkPageQuality(), 'is-crawlable').passed).toBe(false);

    await page.goto('/fx/robots-header?tag=unavailable_after%3A%2025%20Jun%202999%2015%3A00%3A00%20PST');
    expect(audit(await page.checkPageQuality(), 'is-crawlable').passed).toBe(true);
  });

  test('robots.txt disallowing the path blocks it and leaves its neighbours alone', async ({ page }) => {
    await page.goto('/fx/robots-blocked');
    const blocked = audit(await page.checkPageQuality(), 'is-crawlable');
    expect(blocked.passed).toBe(false);
    expect(blocked.items.length).toBe(1);
    expect(blocked.items[0].snippet).toContain('/robots.txt');

    // Same origin, same robots.txt, a path it does not name.
    await page.goto('/fx/landed');
    expect(audit(await page.checkPageQuality(), 'is-crawlable').passed).toBe(true);
  });

  test('robots.txt is fetched for is-crawlable and for nothing else', async ({ page }) => {
    await page.goto('/fx/robots-blocked');
    const fetched: string[] = [];
    page.on('request', (request: any) => {
      if (String(request.url()).endsWith('/robots.txt')) fetched.push(String(request.url()));
    });

    const narrowed = await page.checkPageQuality({ only: ['doctype'] });
    expect(narrowed.audits.map((a) => a.id)).toEqual(['doctype']);
    expect(fetched.length).toBe(0);

    // Nothing on this page objects to being indexed, so a failing
    // verdict is itself the evidence the file was fetched — and
    // counting the request instead would race the event that reports it.
    const crawlable = await page.checkPageQuality({ only: ['is-crawlable'] });
    expect(crawlable.audits.map((a) => a.id)).toEqual(['is-crawlable']);
    expect(crawlable.audits[0].passed).toBe(false);
  });
});
