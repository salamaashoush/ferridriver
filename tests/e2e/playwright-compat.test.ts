// Regression cover for the gaps the Playwright compat harness surfaced
// (see docs/playwright-compat.md). Each test asserts the page- or
// runner-visible effect that only happens when the fix is in, and runs
// on every backend project.

import { test, describe, expect } from '@ferridriver/test';
import type { Download, FileChooser, Page } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

// ── Fixture override / shadowing ──────────────────────────────────────
// `test.extend({ page: async ({ page }, use) => … })` is THE canonical
// Playwright pattern (baseURL, auth state, seeded navigation). The
// same-named dependency must resolve to the parent scope, not to itself.

const seeded = test.extend({
  page: async ({ page }: { page: Page }, use: (p: Page) => Promise<void>) => {
    await page.setContent('<h1 id="seed">seeded</h1>');
    await use(page);
  },
});

// Two overrides deep: each layer sees the one below it, bottom-up.
const seededTwice = seeded.extend({
  page: async ({ page }: { page: Page }, use: (p: Page) => Promise<void>) => {
    await page.evaluate("document.getElementById('seed').textContent += '+outer'");
    await use(page);
  },
});

describe('playwright compat: fixtures', () => {
  seeded('page override shadows the built-in page', async ({ page }) => {
    expect(await page.textContent('#seed')).toBe('seeded');
  });

  seededTwice('override chain runs bottom-up', async ({ page }) => {
    expect(await page.textContent('#seed')).toBe('seeded+outer');
  });

  // A custom fixture depending on the OVERRIDDEN page still gets the
  // override's value, not the built-in.
  const withProbe = seeded.extend({
    probe: async ({ page }: { page: Page }, use: (v: string) => Promise<void>) => {
      await use((await page.textContent('#seed')) ?? '');
    },
  });
  withProbe('custom fixture sees the overridden page', async ({ probe }: { probe: string }) => {
    expect(probe).toBe('seeded');
  });

  // An override with no options tuple inherits scope/auto/option from
  // the registration it shadows, so this stays worker-scoped and is set
  // up once per worker rather than per test.
  const workerScoped = test.extend({
    token: [async ({}, use: (v: string) => Promise<void>) => use('base'), { scope: 'worker' }],
  });
  const workerOverridden = workerScoped.extend({
    token: async ({ token }: { token: string }, use: (v: string) => Promise<void>) => use(`${token}+override`),
  });
  workerOverridden('worker-scoped override inherits its scope', async ({ token }: { token: string }) => {
    expect(token).toBe('base+override');
  });
});

describe('playwright compat: matchers', () => {
  test('toHaveText accepts the array form', async ({ page }) => {
    await page.goto(dataUrl('<ul><li>Open SVG</li><li>Paste markup</li><li>Demo</li></ul>'));
    await expect(page.locator('li')).toHaveText(['Open SVG', 'Paste markup', 'Demo']);
    // Exact length is required, and whitespace is normalized on both sides.
    await expect(page.locator('li')).toHaveText(['  Open SVG ', /Paste/, 'Demo']);
    await expect(page.locator('li')).not.toHaveText(['Open SVG', 'Paste markup']);
    await expect(page.locator('li')).not.toHaveText(['Demo', 'Open SVG', 'Paste markup']);
  });

  test('toContainText array form is an in-order subsequence', async ({ page }) => {
    await page.goto(dataUrl('<ul><li>alpha one</li><li>beta two</li><li>gamma three</li></ul>'));
    // No length requirement, substring match, order enforced.
    await expect(page.locator('li')).toContainText(['alpha', 'gamma']);
    await expect(page.locator('li')).not.toContainText(['gamma', 'alpha']);
  });

  test('toBeChecked reads aria-checked roles and follows labels', async ({ page }) => {
    await page.goto(
      dataUrl(
        '<label class="t" id="wrapped"><input type="checkbox" checked> Show original</label>' +
          '<label class="t" id="wrappedOff"><input type="checkbox"> Compare gzipped</label>' +
          '<div class="t" id="sw" role="switch" aria-checked="true">Multipass</div>' +
          '<div class="t" id="swOff" role="switch" aria-checked="false">Prettify</div>',
      ),
    );
    // A locator landing on the <label> retargets to the control it owns.
    await expect(page.locator('#wrapped')).toBeChecked();
    await expect(page.locator('#wrappedOff')).not.toBeChecked();
    // …and a non-input with an aria-checked role reports its own state.
    await expect(page.locator('#sw')).toBeChecked();
    await expect(page.locator('#swOff')).not.toBeChecked();
    expect(await page.locator('#sw').isChecked()).toBe(true);
  });

  test('toMatchAriaSnapshot matches a partial template', async ({ page }) => {
    await page.goto(
      dataUrl(
        '<h1>Heading</h1>' +
          '<ul><li>Task 1</li><li>Task 2</li><li>Task 3</li></ul>' +
          '<footer><p>unrelated</p></footer>',
      ),
    );
    // Names a SUBSET: the heading, the footer and the extra depth around
    // the list are all absent from the template and must not matter.
    await expect(page.locator('body')).toMatchAriaSnapshot(`
- list:
  - listitem: "Task 1"
  - listitem: "Task 2"
  - listitem: "Task 3"
`);
    // A listitem the page does not have must still fail.
    await expect(page.locator('body')).not.toMatchAriaSnapshot(`
- list:
  - listitem: "Task 9"
`);
  });
});

describe('playwright compat: bindings and files', () => {
  test('exposed function observes its calls in page order', async ({ page }) => {
    const seen: number[] = [];
    await page.exposeFunction('recordCall', (n: number) => {
      seen.push(n);
    });
    await page.goto(dataUrl('<body>ordering</body>'));
    // Fire-and-forget, all in one task: nothing on the page serialises
    // these, so any reordering happens on the driver side.
    await page.evaluate(`
      (() => { for (let i = 0; i < 20; i++) window.recordCall(i); })()
    `);
    await expect
      .poll(async () => seen.length, { timeout: 10000 })
      .toBe(20);
    expect(seen).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19]);
  });

  test('setFiles takes a single FilePayload with a Buffer', async ({ page }) => {
    await page.goto(
      dataUrl(
        '<input id="i" type="file"><button id="b">pick</button><script>' +
          "document.getElementById('b').addEventListener('click', () => document.getElementById('i').click());" +
          "document.getElementById('i').addEventListener('change', async () => {" +
          'const f = document.getElementById("i").files[0];' +
          'document.title = `${f.name}|${f.type}|${await f.text()}`;' +
          '});</script>',
      ),
    );
    const chooserPromise = page.waitForEvent('filechooser', { timeout: 10000 });
    await page.click('#b');
    const chooser = (await chooserPromise) as FileChooser;
    // Not a sequence, and `buffer` is the node-compat Buffer class.
    await chooser.setFiles({
      name: 'file.svg',
      mimeType: 'image/svg+xml',
      buffer: Buffer.from('<svg/>'),
    });
    await expect
      .poll(async () => await page.title(), { timeout: 10000 })
      .toBe('file.svg|image/svg+xml|<svg/>');
  });

  test('fs exposes synchronous reads', async ({ page }) => {
    void page;
    const path = test.info().outputPath('sync-read.txt');
    await fs.writeFile(path, 'sync-payload');
    expect(fs.existsSync(path)).toBe(true);
    expect(fs.readFileSync(path)).toBe('sync-payload');
    expect(fs.readFileBytesSync(path).length).toBe('sync-payload'.length);
    expect(fs.existsSync(test.info().outputPath('absent.txt'))).toBe(false);
  });

  test('a downloaded file is readable at the path download.path() reports', async ({ page }) => {
    await page.goto('/fx/iframe');
    await page.evaluate(
      `const a = document.createElement('a'); a.id = 'dl'; a.href = '/fx/download'; a.textContent = 'dl'; document.body.appendChild(a); null`,
    );
    const downloadPromise = page.waitForEvent('download', { timeout: 15000 });
    await page.click('#dl');
    const download = (await downloadPromise) as Download;
    // The download lands in a backend-owned temp dir, outside the script
    // sandbox root; reading it back is the standard Playwright pattern.
    const downloaded = await download.path();
    expect(fs.readFileSync(downloaded)).toBe('fx-download-payload');
  });
});

describe('playwright compat: frames', () => {
  test('frameLocator re-resolves a late iframe', async ({ page }) => {
    await page.goto('/fx/iframe');
    // The <iframe> only appears after the action starts, so a chain
    // resolved once up front would query a frame that does not exist —
    // or worse, fall back to the main document.
    await page.evaluate(`
      setTimeout(() => {
        const f = document.createElement('iframe');
        f.id = 'late';
        f.srcdoc = '<p id="inner">from the frame</p>';
        document.body.appendChild(f);
      }, 400);
      null
    `);
    const inner = page.frameLocator('#late').locator('#inner');
    expect(await inner.textContent()).toBe('from the frame');
  });
});
