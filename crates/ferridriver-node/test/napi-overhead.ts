/**
 * Measure NAPI boundary overhead: how much time is spent in
 * Rust<->JS serialization vs actual browser work.
 */
import { Browser } from "../index.js";

const RUNS = 100;

async function timeMs(fn: () => Promise<any>): Promise<number> {
  const t: number[] = [];
  // warmup
  for (let i = 0; i < 5; i++) await fn();
  for (let i = 0; i < RUNS; i++) {
    const s = performance.now();
    await fn();
    t.push(performance.now() - s);
  }
  t.sort((a, b) => a - b);
  return t[Math.floor(t.length / 2)];
}

async function main() {
  const browser = await Browser.launch({ backend: "cdp-pipe" });
  const page = await browser.newPageWithUrl("https://example.com");

  // 1. Pure JS evaluate (minimal NAPI overhead - just string in, value out)
  const evalSimple = await timeMs(() => page.evaluate("1+1"));
  console.log(`evaluate('1+1')          ${evalSimple.toFixed(3)}ms`);

  // 2. evaluate returning large string (tests serialization of large return values)
  const evalLarge = await timeMs(() => page.evaluate("'x'.repeat(10000)"));
  console.log(`evaluate(10KB string)    ${evalLarge.toFixed(3)}ms`);

  const evalHuge = await timeMs(() => page.evaluate("'x'.repeat(100000)"));
  console.log(`evaluate(100KB string)   ${evalHuge.toFixed(3)}ms`);

  // 3. evaluate returning object (tests JSON serialization)
  const evalObj = await timeMs(() => page.evaluate("({a:1,b:'hello',c:[1,2,3],d:{nested:true}})"));
  console.log(`evaluate(object)         ${evalObj.toFixed(3)}ms`);

  // 4. Locator property read (NAPI call -> Rust -> CDP evaluate -> parse result -> NAPI return)
  const locText = await timeMs(() => page.locator("h1").textContent());
  console.log(`loc.textContent()        ${locText.toFixed(3)}ms`);

  // 5. Screenshot (tests Buffer transfer overhead)
  const screenshotSmall = await timeMs(() => page.screenshotElement("h1"));
  console.log(`screenshot(h1 element)   ${screenshotSmall.toFixed(3)}ms`);

  const screenshot = await timeMs(() => page.screenshot());
  console.log(`screenshot(full page)    ${screenshot.toFixed(3)}ms`);

  // 6. Cookies (tests object array serialization)
  await page.setCookie({ name: "a", value: "1", domain: ".example.com", path: "/", secure: false, httpOnly: false });
  await page.setCookie({ name: "b", value: "2", domain: ".example.com", path: "/", secure: false, httpOnly: false });
  await page.setCookie({ name: "c", value: "3", domain: ".example.com", path: "/", secure: false, httpOnly: false });
  const getCookies = await timeMs(() => page.cookies());
  console.log(`cookies() (3 cookies)    ${getCookies.toFixed(3)}ms`);

  // 7. Multiple rapid calls (tests NAPI call overhead amortized)
  const rapidCalls = await timeMs(async () => {
    await page.title();
    await page.url();
    await page.evaluate("document.readyState");
  });
  console.log(`3x rapid calls           ${rapidCalls.toFixed(3)}ms`);

  // 8. No-op-like calls to isolate NAPI boundary cost
  // title() is the lightest - just CDP Runtime.evaluate("document.title")
  const titleCall = await timeMs(() => page.title());
  console.log(`title() (lightest call)  ${titleCall.toFixed(3)}ms`);

  // 9. Compare: same evaluate from JS directly vs through locator
  const directEval = await timeMs(() => page.evaluate("document.querySelector('h1')?.textContent"));
  const locatorPath = await timeMs(() => page.locator("h1").textContent());
  console.log(`\ndirect evaluate h1.text  ${directEval.toFixed(3)}ms`);
  console.log(`locator('h1').text       ${locatorPath.toFixed(3)}ms`);
  console.log(`locator overhead         ${(locatorPath - directEval).toFixed(3)}ms (${((locatorPath/directEval - 1)*100).toFixed(0)}% more)`);

  await browser.close();
}

main().catch(console.error);
