// `page.checkPageQuality()` — the Lighthouse audits that score a page
// as it stands, ported in `ferridriver::audits`. Seven of them
// Lighthouse also scores in snapshot mode; `canonical` it scores only
// after a navigation, which is why the recorded gate keeps a second
// recording per fixture page.
//
// What these agree with is checked elsewhere: `just quality-diff` compares
// the verdicts against Lighthouse's on the same pages and
// `cargo test -p ferridriver-perf --test lighthouse` gates it. Here is
// where they run on every backend, because the artifacts come out of one
// evaluate returning a JSON string and each backend serialises that
// differently — and because `preventsPaste` is not a read but a
// synthetic ClipboardEvent, which is exactly the kind of thing that
// works on one engine and not another.

import { test, describe, expect } from '@ferridriver/test';
import type { PageQualityReport } from '@ferridriver/test';

// setContent leaves the document at about:blank, where a relative href
// resolves to nothing and `crawlable-anchors` fails it — which is what
// Lighthouse concludes there too. Absolute hrefs keep these specs about
// the audits rather than about the document URL.
const AUDIT_IDS = [
  'doctype',
  'meta-description',
  'canonical',
  'crawlable-anchors',
  'link-text',
  'image-aspect-ratio',
  'image-size-responsive',
  'paste-preventing-inputs',
];

const failing = (report: PageQualityReport) => report.audits.filter((a) => !a.passed).map((a) => a.id);
const audit = (report: PageQualityReport, id: string) => report.audits.find((a) => a.id === id)!;

describe('page.checkPageQuality', () => {
  test('reports all seven, with a category and a title on each', async ({ page }) => {
    await page.setContent(
      "<!doctype html><html lang='en'><head><title>t</title>" +
        "<meta name='description' content='A page with a description'></head>" +
        "<body><main><p><a href='https://example.com/pricing'>See our pricing</a></p></main></body></html>",
    );
    const report = await page.checkPageQuality();

    expect(report.audits.map((a) => a.id)).toEqual(AUDIT_IDS);
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
});
