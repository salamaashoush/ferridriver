// Render the image pairs the screenshot comparator is checked on.
//
//   ferridriver run --no-inherit "$PWD/scripts/comparator-diff/capture.ts" -- <out-dir>
//
// Each case is a directory holding expected.png and actual.png. They are
// real browser renders, so the anti-aliased edges the comparators treat
// specially are the ones a screenshot assertion actually meets.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

// `ferridriver run` hands the arguments after `--` to the script as `args`.
const out = (globalThis as { args?: string[] }).args?.[0] ?? 'crates/ferridriver-test/tests/fixtures/comparator';

const text = (left: number, colour = '#222') => `<style>html,body{margin:0;background:#fff}
  p{position:absolute;left:${left}px;top:8px;margin:0;font:16px/20px Helvetica,Arial,sans-serif;color:${colour}}</style>
  <p>The quick brown fox jumps over the lazy dog 0123456789</p>`;
const block = (colour: string) => `<style>html,body{margin:0;background:#fff}</style>
  <div style="position:absolute;left:20px;top:10px;width:80px;height:30px;background:${colour}"></div>`;
const circle = (radius: number) => `<style>html,body{margin:0;background:#fff}</style>
  <div style="position:absolute;left:${60 - radius}px;top:${30 - radius}px;width:${radius * 2}px;height:${radius * 2}px;border-radius:50%;background:#1a6"></div>`;
const gradient = (stop: string) => `<style>html,body{margin:0}</style>
  <div style="width:160px;height:60px;background:linear-gradient(90deg,#0af,${stop},#f60)"></div>`;
const translucent = (alpha: number) => `<style>html,body{margin:0;background:transparent}</style>
  <div style="position:absolute;left:10px;top:10px;width:100px;height:40px;background:rgba(200,30,90,${alpha})"></div>`;

type Shot = { html: string; engine?: 'chromium' | 'webkit'; width?: number; height?: number; transparent?: boolean };
const cases: Record<string, [Shot, Shot]> = {
  identical: [{ html: text(10) }, { html: text(10) }],
  'text-shift': [{ html: text(10) }, { html: text(11) }],
  'text-colour': [{ html: text(10, '#222') }, { html: text(10, '#2a2a2a') }],
  'text-engines': [{ html: text(10), engine: 'chromium' }, { html: text(10), engine: 'webkit' }],
  'block-subtle': [{ html: block('#808080') }, { html: block('#828282') }],
  'block-visible': [{ html: block('#808080') }, { html: block('#a08080') }],
  'circle-radius': [{ html: circle(20) }, { html: circle(21) }],
  'gradient-stop': [{ html: gradient('#fff') }, { html: gradient('#fef') }],
  'alpha-blend': [{ html: translucent(0.5), transparent: true }, { html: translucent(0.6), transparent: true }],
  'size-mismatch': [{ html: block('#808080'), width: 160, height: 60 }, { html: block('#808080'), width: 150, height: 70 }],
};

const browsers = { chromium: await chromium().launch(), webkit: await webkit().launch() };
try {
  for (const [name, pair] of Object.entries(cases)) {
    const dir = join(out, name);
    mkdirSync(dir, { recursive: true });
    for (const [file, shot] of [['expected.png', pair[0]], ['actual.png', pair[1]]] as const) {
      const browser = browsers[shot.engine ?? 'chromium'];
      const context = await browser.newContext({ viewport: { width: shot.width ?? 160, height: shot.height ?? 60 } });
      const page = await context.newPage();
      await page.setContent(shot.html);
      writeFileSync(join(dir, file), await page.screenshot({ omitBackground: shot.transparent ?? false }));
      await context.close();
    }
  }
} finally {
  await browsers.chromium.close();
  await browsers.webkit.close();
}
console.log(`wrote ${Object.keys(cases).length} cases to ${out}`);
