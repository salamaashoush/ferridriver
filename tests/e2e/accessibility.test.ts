// `page.checkAccessibility()` — axe-core, run inside the page.
//
// Not a Playwright method: Playwright's `page.accessibility` is the
// accessibility TREE, a different question. This runs the engine
// Lighthouse's 67 accessibility audits wrap and reports what it found.
//
// Here on every backend rather than only through NAPI, because the
// audit is one evaluate carrying a megabyte of script and returning a
// stringified result, and each backend serialises that differently.
// What it agrees with is checked elsewhere: `just a11y-diff` compares
// the verdicts against Lighthouse's on the same pages, and
// `cargo test -p ferridriver-perf --test lighthouse` gates it.
//
// Needs the engine on disk: `ferridriver install axe`.

import { test, describe, expect } from '@ferridriver/test';

// An image with no alt text, inside a document with no title and no
// lang: three rules that axe fires on whatever the engine underneath.
const BROKEN = "<html><body><img src='data:,x'><p>text</p></body></html>";

describe('page.checkAccessibility', () => {
  test('reports the rules that failed, the elements, and the engine', async ({ page }) => {
    await page.setContent(BROKEN);
    const report = await page.checkAccessibility();

    expect(report.engine).toMatch(/^axe-core\/\d+\.\d+\.\d+$/);
    const failed = report.violations.map((v) => v.id);
    expect(failed).toContain('image-alt');
    expect(failed).toContain('html-has-lang');
    expect(failed).toContain('document-title');

    const imageAlt = report.violations.find((v) => v.id === 'image-alt')!;
    expect(imageAlt.impact).toBe('critical');
    expect(imageAlt.helpUrl).toContain('dequeuniversity.com');
    // The element and the reason, which is the part anyone acts on.
    expect(imageAlt.nodes.length).toBe(1);
    expect(imageAlt.nodes[0].target[0]).toContain('img');
    expect(imageAlt.nodes[0].failureSummary.length).toBeGreaterThan(0);
  });

  test('four outcomes, so a clean page is distinguishable from an unrun one', async ({ page }) => {
    await page.setContent(
      "<html lang='en'><head><title>Fine</title></head><body><main><h1>Fine</h1><p>ok</p></main></body></html>",
    );
    const report = await page.checkAccessibility();
    expect(report.passes.length).toBeGreaterThan(0);
    // A rule with nothing to look at ran and found nothing, which is not
    // the same as a rule that never ran.
    expect(report.inapplicable.length).toBeGreaterThan(0);
    expect(report.violations.map((v) => v.id)).not.toContain('html-has-lang');
  });

  test('runs the rules axe hides unless asked, which Lighthouse asks for', async ({ page }) => {
    // `target-size` ships `enabled: false` and `td-has-header` is tagged
    // `experimental`, so axe's defaults drop both. Lighthouse enables
    // them, so an audit claiming to cover what Lighthouse covers has to
    // reach them too — the page here is irrelevant, only that the rules
    // were evaluated at all.
    await page.setContent("<html lang='en'><head><title>t</title></head><body><p>x</p></body></html>");
    const report = await page.checkAccessibility();
    const evaluated = new Set(
      [...report.violations, ...report.passes, ...report.incomplete, ...report.inapplicable].map((r) => r.id),
    );
    for (const rule of ['target-size', 'td-has-header', 'table-fake-caption', 'identical-links-same-purpose']) {
      expect(evaluated.has(rule)).toBe(true);
    }
  });

  test('tags narrow which rules run', async ({ page }) => {
    await page.setContent(BROKEN);
    const all = await page.checkAccessibility();
    const wcag2a = await page.checkAccessibility({ tags: ['wcag2a'] });

    // Every rule that survived the filter carries the tag, and the
    // filter actually removed something.
    for (const rule of wcag2a.violations) expect(rule.tags).toContain('wcag2a');
    const allIds = new Set(all.violations.map((v) => v.id));
    const taggedIds = new Set(wcag2a.violations.map((v) => v.id));
    expect(taggedIds.size).toBeLessThan(allIds.size);
    for (const id of taggedIds) expect(allIds.has(id)).toBe(true);
  });

  test('include narrows the audit to a subtree, exclude cuts one out', async ({ page }) => {
    await page.setContent(
      "<html lang='en'><head><title>t</title></head><body>" +
        "<div id='left'><img src='data:,a'></div><div id='right'><img src='data:,b'></div>" +
        '</body></html>',
    );
    const count = (report: { violations: { id: string; nodes: unknown[] }[] }) =>
      report.violations.find((v) => v.id === 'image-alt')?.nodes.length ?? 0;

    expect(count(await page.checkAccessibility())).toBe(2);
    expect(count(await page.checkAccessibility({ include: ['#left'] }))).toBe(1);
    expect(count(await page.checkAccessibility({ exclude: ['#left'] }))).toBe(1);
  });
});
