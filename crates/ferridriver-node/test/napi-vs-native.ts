/**
 * Compare NAPI overhead vs raw CDP latency.
 * Measures the same operations from JS (through NAPI) and estimates
 * how much of the time is NAPI vs actual Chrome work.
 *
 * We do this by measuring:
 * 1. A raw CDP command (page.evaluate) - this is NAPI + Rust + CDP + Chrome + return
 * 2. Multiple sequential CDP commands - to see if NAPI overhead compounds
 * 3. A pure NAPI call with no CDP (create a locator - synchronous, no browser)
 */
import { Browser } from "../index.js";

const RUNS = 500;

function medianNs(fn: () => any): number {
  // warmup
  for (let i = 0; i < 20; i++) fn();
  const t: number[] = [];
  for (let i = 0; i < RUNS; i++) {
    const s = Bun.nanoseconds();
    fn();
    t.push(Bun.nanoseconds() - s);
  }
  t.sort((a, b) => a - b);
  return t[Math.floor(t.length / 2)];
}

async function medianAsync(fn: () => Promise<any>): Promise<number> {
  for (let i = 0; i < 20; i++) await fn();
  const t: number[] = [];
  for (let i = 0; i < RUNS; i++) {
    const s = Bun.nanoseconds();
    await fn();
    t.push(Bun.nanoseconds() - s);
  }
  t.sort((a, b) => a - b);
  return t[Math.floor(t.length / 2)];
}

async function main() {
  const browser = await Browser.launch({ backend: "cdp-pipe" });
  const page = await browser.newPageWithUrl("https://example.com");

  console.log("=== SYNCHRONOUS NAPI CALLS (no CDP, pure NAPI boundary cost) ===\n");

  // Pure sync NAPI: locator creation (no CDP, just Rust string ops)
  const locCreate = medianNs(() => page.locator("h1"));
  console.log(`page.locator("h1")       ${(locCreate/1000).toFixed(1)}us  (sync, no CDP)`);

  // Locator chaining
  const locChain = medianNs(() => page.locator("div").locator("h1").first());
  console.log(`loc.loc.first()          ${(locChain/1000).toFixed(1)}us  (3 sync NAPI calls)`);

  // Getter
  const locSelector = medianNs(() => page.locator("h1").selector);
  console.log(`loc.selector (getter)    ${(locSelector/1000).toFixed(1)}us  (sync getter)`);

  // setDefaultTimeout (sync, no CDP)
  const setTimeout = medianNs(() => page.setDefaultTimeout(5000));
  console.log(`setDefaultTimeout()      ${(setTimeout/1000).toFixed(1)}us  (sync setter)`);

  console.log("\n=== ASYNC NAPI CALLS (NAPI + Rust + CDP round-trip) ===\n");

  // Lightest CDP call
  const titleNs = await medianAsync(() => page.title());
  console.log(`title()                  ${(titleNs/1000).toFixed(1)}us  (1 CDP call)`);

  const evalNs = await medianAsync(() => page.evaluate("1"));
  console.log(`evaluate("1")            ${(evalNs/1000).toFixed(1)}us  (1 CDP call)`);

  // Our optimized single-call locator op
  const locTextNs = await medianAsync(() => page.locator("h1").textContent());
  console.log(`loc.textContent()        ${(locTextNs/1000).toFixed(1)}us  (sync locator + 1 CDP call)`);

  const locVisNs = await medianAsync(() => page.locator("h1").isVisible());
  console.log(`loc.isVisible()          ${(locVisNs/1000).toFixed(1)}us  (sync locator + 1 CDP call)`);

  const locCountNs = await medianAsync(() => page.locator("p").count());
  console.log(`loc.count()              ${(locCountNs/1000).toFixed(1)}us  (sync locator + 1 CDP call)`);

  // Fill (1 CDP call now)
  const fillNs = await medianAsync(() => page.fill("#name" ,"x"));
  console.log(`fill("#name","x")        ${(fillNs/1000).toFixed(1)}us  (1 CDP call)`);

  console.log("\n=== OVERHEAD ANALYSIS ===\n");

  const napiSyncCost = locCreate / 1000; // us
  const cdpRoundTrip = evalNs / 1000; // us
  const napiAsyncOverhead = cdpRoundTrip - 50; // ~50us estimated raw pipe latency

  console.log(`NAPI sync call cost:     ~${napiSyncCost.toFixed(1)}us`);
  console.log(`CDP round-trip (pipe):   ~${cdpRoundTrip.toFixed(0)}us`);
  console.log(`Locator overhead vs raw: ~${((locTextNs - evalNs)/1000).toFixed(1)}us (selector engine JS parsing)`);
  console.log(`\nBreakdown of loc.textContent() = ${(locTextNs/1000).toFixed(0)}us:`);
  console.log(`  NAPI sync (locator):   ~${napiSyncCost.toFixed(0)}us`);
  console.log(`  CDP evaluate:          ~${cdpRoundTrip.toFixed(0)}us`);
  console.log(`  Selector engine JS:    ~${((locTextNs - evalNs - locCreate)/1000).toFixed(0)}us`);

  await browser.close();
}

main().catch(console.error);
