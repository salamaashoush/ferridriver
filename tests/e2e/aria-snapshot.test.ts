// `locator.ariaSnapshot()` — Playwright's accessibility YAML, rendered
// by the vendored injected renderer (tree -> JSON -> YAML since 1.63)
// and stitched across iframes by the Rust host.
//
// The stitch is the ferridriver-specific half: Playwright builds its
// injected options per frame and mints `f<seq>e<n>` refs inside the
// renderer, while ferridriver injects one script per page and prefixes
// a child frame's refs when it splices that frame's render under the
// parent's `- iframe [ref=...]` line. Both halves are asserted here.

import { test, describe, expect } from '@ferridriver/test';
import { setBody } from './helpers/html';

describe('locator.ariaSnapshot', () => {
  test('renders roles, names and text', async ({ page }) => {
    await setBody(
      page,
      `<main>
         <h1>Title</h1>
         <button>Go</button>
         <a href="https://example.com/">Link</a>
       </main>`,
    );
    const snapshot = await page.locator('main').ariaSnapshot();
    expect(snapshot).toContain('- heading "Title" [level=1]');
    expect(snapshot).toContain('- button "Go"');
    expect(snapshot).toContain('- link "Link"');
    // A link renders its href as a `/url` property line.
    expect(snapshot).toContain('- /url: https://example.com/');
  });

  test('refs are minted in ai mode only', async ({ page }) => {
    await setBody(page, '<button>Press</button>');
    const ai = await page.locator('body').ariaSnapshot({ mode: 'ai' });
    const dflt = await page.locator('body').ariaSnapshot();
    expect(ai).toMatch(/- button "Press" \[ref=e\d+\]/);
    expect(dflt).toContain('- button "Press"');
    expect(dflt).not.toContain('[ref=');
  });

  test('depth stops the render', async ({ page }) => {
    await setBody(
      page,
      `<main><section><article><button>Deep</button></article></section></main>`,
    );
    const shallow = await page.locator('main').ariaSnapshot({ depth: 1 });
    const full = await page.locator('main').ariaSnapshot();
    expect(full).toContain('Deep');
    expect(shallow).not.toContain('Deep');
  });

  test('an iframe subtree is spliced under its own line, with its own ref namespace', async ({ page }) => {
    await setBody(
      page,
      `<button>outer</button>
       <iframe id="f" srcdoc="<button>inner</button>"></iframe>`,
    );
    const snapshot = await page.locator('body').ariaSnapshot({ mode: 'ai' });
    const lines = snapshot.split('\n');

    const iframeIndex = lines.findIndex(l => /- iframe .*\[ref=/.test(l));
    expect(iframeIndex).toBeGreaterThanOrEqual(0);
    const innerIndex = lines.findIndex(l => l.includes('"inner"'));
    // The child's button is BELOW the iframe line and indented deeper —
    // i.e. spliced into the parent render, not appended after it.
    expect(innerIndex).toBeGreaterThan(iframeIndex);
    const indentOf = (l: string) => l.length - l.trimStart().length;
    expect(indentOf(lines[innerIndex])).toBeGreaterThan(indentOf(lines[iframeIndex]));

    // Every ref in the child frame carries the frame prefix, and no ref
    // is repeated across the two frames.
    const refs = [...snapshot.matchAll(/\[ref=([^\]]+)\]/g)].map(m => m[1]);
    expect(refs.length).toBeGreaterThan(2);
    expect(new Set(refs).size).toBe(refs.length);
    const innerRef = /\[ref=([^\]]+)\]/.exec(lines[innerIndex])?.[1];
    expect(innerRef).toMatch(/^f\d+e\d+$/);
    const outerRef = /\[ref=([^\]]+)\]/.exec(lines[iframeIndex])?.[1];
    expect(outerRef).toMatch(/^e\d+$/);
  });

  test('the element scopes the snapshot', async ({ page }) => {
    await setBody(page, '<div id="a"><button>in</button></div><button>out</button>');
    const scoped = await page.locator('#a').ariaSnapshot();
    expect(scoped).toContain('"in"');
    expect(scoped).not.toContain('"out"');
  });
});
