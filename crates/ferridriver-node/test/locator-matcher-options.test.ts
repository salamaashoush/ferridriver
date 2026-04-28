// Cluster 5 — locator matcher option bags (§7.17).
//
// Exercises the new option fields against a live cdp-pipe browser:
//   toBeInViewport({ ratio })
//   toHaveCSS({ pseudo })
//   toHaveScreenshot({ threshold, maxDiffPixels, maxDiffPixelRatio, ignore })
//   toMatchAriaSnapshot (improved structural-by-line match)

import { test, expect as bunExpect } from 'bun:test';
import { mkdtempSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { chromium, type Browser } from '../index.js';
import { expect } from '../../../packages/ferridriver-test/src/expect';

let browser: Browser;

async function withPage(html: string, fn: (page: any) => Promise<void>): Promise<void> {
  if (!browser) browser = await chromium().launch();
  const ctx = browser.newContext();
  try {
    const page = await ctx.newPage();
    await page.setContent(html);
    await fn(page);
  } finally {
    await ctx.close();
  }
}

test('toBeInViewport without ratio accepts any overlap', async () => {
  await withPage(
    `<style>body { margin: 0; height: 5000px; }</style>
     <div id="hit" style="height: 100px"></div>`,
    async (page) => {
      const loc = page.locator('#hit');
      await expect(loc).toBeInViewport();
    },
  );
});

test('toBeInViewport({ ratio: 1 }) requires the full element visible', async () => {
  await withPage(
    `<style>body { margin: 0; height: 200px; }</style>
     <div id="big" style="height: 2000px"></div>`,
    async (page) => {
      const loc = page.locator('#big');
      // Only a fraction fits — drop the timeout so the failing
      // assertion bails fast rather than polling for the default 5s.
      await bunExpect(async () => expect(loc, 500).toBeInViewport({ ratio: 1 })).toThrow();
      await expect(loc).toBeInViewport({ ratio: 0.05 });
    },
  );
}, 10_000);

test('toHaveCSS({ pseudo }) targets ::before', async () => {
  await withPage(
    `<style>
       #x::before { content: "marker"; color: rgb(10, 20, 30); }
     </style>
     <div id="x"></div>`,
    async (page) => {
      const loc = page.locator('#x');
      await expect(loc).toHaveCSS('color', 'rgb(10, 20, 30)', { pseudo: '::before' });
    },
  );
});

test('toHaveScreenshot({ ignore: true }) short-circuits comparison', async () => {
  const snapDir = mkdtempSync(join(tmpdir(), 'ferri-cluster5-snap-'));
  process.env.SNAPSHOT_DIR = snapDir;
  try {
    await withPage(
      `<div id="box" style="width:50px;height:50px;background:red"></div>`,
      async (page) => {
        const loc = page.locator('#box');
        // Write a deliberately-wrong baseline so a non-ignored compare
        // would fail.
        writeFileSync(join(snapDir, 'wrong.png'), Buffer.from('not a png'));
        await expect(loc).toHaveScreenshot('wrong', { ignore: true });
      },
    );
  } finally {
    delete process.env.SNAPSHOT_DIR;
    rmSync(snapDir, { recursive: true, force: true });
  }
});

test('toMatchAriaSnapshot accepts ordered subset', async () => {
  await withPage(
    `<main>
       <h1 aria-label="Title">Title</h1>
       <button>Click</button>
       <ul>
         <li>One</li>
         <li>Two</li>
       </ul>
     </main>`,
    async (page) => {
      const loc = page.locator('main');
      await expect(loc).toMatchAriaSnapshot(`
        h1 "Title"
        button "Click"
        li "One"
        li "Two"
      `);
    },
  );
});

test('toMatchAriaSnapshot rejects out-of-order expectations', async () => {
  await withPage(
    `<main>
       <h1>First</h1>
       <button>Second</button>
     </main>`,
    async (page) => {
      const loc = page.locator('main');
      // Rejecting reversed order is what the new structural-cursor
      // walker buys us over the old line-substring loop. Drop the
      // timeout so the assertion bails fast rather than polling for
      // the default 5s.
      await bunExpect(async () =>
        expect(loc, 500).toMatchAriaSnapshot(`
          button "Second"
          h1 "First"
        `),
      ).toThrow();
    },
  );
});
