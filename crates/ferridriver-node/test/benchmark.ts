/**
 * Performance benchmark: ferridriver backends vs Playwright
 *
 * Measures per-operation median latency across all backends.
 * Each backend gets its own browser instance to avoid interference.
 */

import { type Browser as FdBrowser } from "../index.js";
import { launchForBackend as fdLaunchForBackend } from "./_helpers.js";
import { chromium } from "playwright";

const WARMUP = 3;
const RUNS = 15;

const FD_BACKENDS = ["cdp-pipe", "cdp-raw"] as const;
if (process.platform === "darwin") {
  (FD_BACKENDS as unknown as string[]).push("webkit");
}

interface Agg {
  median: number;
  mean: number;
  samples: number;
}

async function measure(fn: () => Promise<void>, reset?: () => Promise<void>): Promise<Agg> {
  for (let i = 0; i < WARMUP; i++) {
    if (reset) await reset();
    try { await fn(); } catch {}
  }
  const t: number[] = [];
  for (let i = 0; i < RUNS; i++) {
    if (reset) await reset();
    const s = performance.now();
    try {
      await fn();
      t.push(performance.now() - s);
    } catch {}
  }
  if (t.length === 0) return { median: -1, mean: -1, samples: 0 }; // all failed
  t.sort((a, b) => a - b);
  const median = t[Math.floor(t.length / 2)];
  const mean = t.reduce((s, v) => s + v, 0) / t.length;
  return { median, mean, samples: t.length };
}

interface Row {
  op: string;
  playwright: number;
  playwrightMean: number;
  [backend: string]: number | string;
}

const HTML = `<html><body>
  <h1>Benchmark</h1>
  <p>Text content.</p><p>More text.</p>
  <form>
    <input type="text" id="name" />
    <input type="checkbox" id="agree" />
    <button id="btn" onclick="document.getElementById('r').textContent='ok'">Go</button>
  </form>
  <div id="r"></div>
  <ul>${Array.from({ length: 50 }, (_, i) => `<li>Item ${i}</li>`).join("")}</ul>
</body></html>`;

type BenchFn = (page: any, label: string) => Promise<number>;

// "reset" ops re-set the HTML before each iteration to keep state clean.
const ops: { name: string; reset?: boolean; fd: (p: any) => Promise<void>; pw: (p: any) => Promise<void> }[] = [
  // Navigation
  { name: "goto (network)", fd: p => p.goto("https://example.com"), pw: p => p.goto("https://example.com") },
  { name: "setContent", fd: p => p.setContent(HTML), pw: p => p.setContent(HTML) },
  // Content extraction
  { name: "title()", fd: p => p.title(), pw: p => p.title() },
  { name: "content()", fd: p => p.content(), pw: p => p.content() },
  { name: "innerText('h1')", fd: p => p.innerText("h1"), pw: p => p.innerText("h1") },
  { name: "innerHTML('ul')", fd: p => p.innerHtml("ul"), pw: p => p.innerHTML("ul") },
  // JS evaluation
  { name: "evaluate('1+1')", fd: p => p.evaluate("1+1"), pw: p => p.evaluate("1+1") },
  { name: "evaluate (50 elems)", fd: p => p.evaluate("Array.from(document.querySelectorAll('li')).map(e=>e.textContent)"), pw: p => p.evaluate("Array.from(document.querySelectorAll('li')).map(e=>e.textContent)") },
  // Locator
  { name: "loc('h1').textContent()", fd: p => p.locator("h1").textContent(), pw: p => p.locator("h1").textContent() },
  { name: "loc('li').count()", fd: p => p.locator("li").count(), pw: p => p.locator("li").count() },
  { name: "loc('h1').isVisible()", fd: p => p.locator("h1").isVisible(), pw: p => p.locator("h1").isVisible() },
  { name: "loc('h1').boundingBox()", fd: p => p.locator("h1").boundingBox(), pw: p => p.locator("h1").boundingBox() },
  { name: "loc('li').allTextContents()", fd: p => p.locator("li").allTextContents(), pw: p => p.locator("li").allTextContents() },
  // Actions - reset content each iteration to keep DOM clean.
  // Both sides MUST use the SAME flags or the comparison is confounded. We
  // previously passed force:true ONLY to Playwright (to skip actionability
  // waits that hang after rapid setContent), which made the reported click()
  // ratio meaningless. Both sides now go through the locator API with
  // force:true so each skips the same actionability checks. See BENCHMARKING.md.
  { name: "fill('#name', text)", reset: true, fd: p => p.locator("#name").fill("bench", { force: true }), pw: p => p.locator("#name").fill("bench", { force: true }) },
  { name: "click('#btn')", reset: true, fd: p => p.locator("#btn").click({ force: true }), pw: p => p.locator("#btn").click({ force: true }) },
  { name: "check('#agree')", reset: true, fd: p => p.locator("#agree").check({ force: true }), pw: p => p.locator("#agree").check({ force: true }) },
  // Screenshots
  { name: "screenshot()", fd: p => p.screenshot(), pw: p => p.screenshot() },
  { name: "screenshot(fullPage)", fd: p => p.screenshot({ fullPage: true }), pw: p => p.screenshot({ fullPage: true }) },
  // Viewport
  { name: "setViewportSize()", fd: p => p.setViewportSize(1024, 768), pw: p => p.setViewportSize({ width: 1024, height: 768 }) },
];

async function main() {
  // ── Launch all browsers ───────────────────────────────────────────────
  console.log("Launching browsers...");

  const pwBrowser = await chromium.launch();
  const pwPage = await pwBrowser.newPage();
  await pwPage.setContent(HTML);

  const fdBrowsers: { backend: string; browser: FdBrowser; page: any }[] = [];
  for (const backend of FD_BACKENDS) {
    const browser = await fdLaunchForBackend(backend);
    const page = await browser.newPage();
    await page.goto("https://example.com"); // initial load
    await page.setContent(HTML);
    fdBrowsers.push({ backend, browser, page });
    console.log(`  ${backend} ready`);
  }
  console.log(`  playwright ready`);

  // ── Run benchmarks ────────────────────────────────────────────────────
  const rows: Row[] = [];

  for (const op of ops) {
    process.stdout.write(`\n${op.name.padEnd(30)}`);

    const row: Row = { op: op.name, playwright: 0, playwrightMean: 0 };

    // Playwright
    const pwReset = op.reset ? () => pwPage.setContent(HTML) : undefined;
    const pw = await measure(() => op.pw(pwPage), pwReset);
    row.playwright = +pw.median.toFixed(2);
    row.playwrightMean = +pw.mean.toFixed(2);
    process.stdout.write(` pw:${pw.median.toFixed(1).padStart(7)}ms`);

    // Each ferridriver backend
    for (const { backend, page } of fdBrowsers) {
      const fdReset = op.reset ? () => page.setContent(HTML) : undefined;
      const fd = await measure(() => op.fd(page), fdReset);
      row[backend] = +fd.median.toFixed(2);
      row[`${backend}_mean`] = +fd.mean.toFixed(2);
      const ratio = pw.median / fd.median;
      const tag = ratio > 1 ? `${ratio.toFixed(1)}x` : `${(1/ratio).toFixed(1)}x slow`;
      process.stdout.write(`  ${backend}:${fd.median.toFixed(1).padStart(7)}ms (${tag})`);
    }

    rows.push(row);
  }

  // ── Summary table ─────────────────────────────────────────────────────
  const backendNames = FD_BACKENDS as unknown as string[];
  const colW = 12;

  console.log("\n\n" + "=".repeat(30 + (backendNames.length + 1) * colW));
  console.log(
    "Operation".padEnd(30) +
    "Playwright".padStart(colW) +
    backendNames.map(b => b.padStart(colW)).join("")
  );
  console.log("-".repeat(30 + (backendNames.length + 1) * colW));

  for (const r of rows) {
    let line = r.op.padEnd(30) + `${r.playwright}ms`.padStart(colW);
    for (const b of backendNames) {
      const v = r[b] as number;
      const ratio = r.playwright / v;
      const tag = ratio > 1 ? `${ratio.toFixed(1)}x` : `1/${(1/ratio).toFixed(1)}x`;
      line += `${v}ms ${tag}`.padStart(colW);
    }
    console.log(line);
  }

  // Totals
  console.log("-".repeat(30 + (backendNames.length + 1) * colW));
  const pwTotal = rows.reduce((s, r) => s + r.playwright, 0);
  let totalLine = "TOTAL".padEnd(30) + `${pwTotal.toFixed(1)}ms`.padStart(colW);
  for (const b of backendNames) {
    const t = rows.reduce((s, r) => s + (r[b] as number), 0);
    const ratio = pwTotal / t;
    const tag = ratio > 1 ? `${ratio.toFixed(1)}x faster` : `${(1/ratio).toFixed(1)}x slower`;
    totalLine += `${t.toFixed(1)}ms ${tag}`.padStart(colW);
  }
  console.log(totalLine);

  // ── CSV ───────────────────────────────────────────────────────────────
  // Emit both median and mean per op so the aggregation is auditable and no
  // downstream claim can cherry-pick a single statistic. See BENCHMARKING.md.
  const header = [
    "operation",
    "playwright_median_ms",
    "playwright_mean_ms",
    ...backendNames.flatMap(b => [`${b}_median_ms`, `${b}_mean_ms`]),
  ].join(",");
  const csvRows = rows.map(r =>
    [
      r.op,
      r.playwright,
      r.playwrightMean,
      ...backendNames.flatMap(b => [r[b], r[`${b}_mean`]]),
    ].join(",")
  );
  const csv = [header, ...csvRows].join("\n");
  await Bun.write("test/benchmark-results.csv", csv);
  console.log("\nResults saved to test/benchmark-results.csv");

  // ── Cleanup ───────────────────────────────────────────────────────────
  for (const { browser } of fdBrowsers) await browser.close();
  await pwBrowser.close();
}

main().catch(console.error);
