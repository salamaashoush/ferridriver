import { Browser } from "../index.js";

const RUNS = 50;
async function med(fn: () => Promise<any>): Promise<number> {
  for (let i = 0; i < 5; i++) await fn();
  const t: number[] = [];
  for (let i = 0; i < RUNS; i++) {
    const s = performance.now();
    await fn();
    t.push(performance.now() - s);
  }
  t.sort((a, b) => a - b);
  return t[Math.floor(t.length / 2)];
}

async function bench(backend: string) {
  const b = await Browser.launch({ backend });
  const p = await b.newPageWithUrl("https://example.com");
  
  // Single CDP call baseline
  const evalBase = await med(() => p.evaluate("1"));
  
  // goto breakdown - this is the biggest gap vs Playwright
  const goto = await med(() => p.goto("https://example.com"));
  
  // click breakdown - cdp-ws is 98ms, others 3ms
  await p.setContent('<button id="b">x</button>');
  const click = await med(() => p.click("#b"));
  
  // setContent - now slower because of engine inject
  const setContent = await med(() => p.setContent("<h1>hi</h1>"));
  
  // screenshot
  const screenshot = await med(() => p.screenshot());
  
  // fill (needs input)
  await p.setContent('<input id="i"/>');
  const fill = await med(() => p.fill("#i", "x"));

  console.log(`[${backend.padEnd(8)}] eval:${evalBase.toFixed(1).padStart(6)}ms  goto:${goto.toFixed(1).padStart(6)}ms  click:${click.toFixed(1).padStart(6)}ms  setContent:${setContent.toFixed(1).padStart(6)}ms  screenshot:${screenshot.toFixed(1).padStart(6)}ms  fill:${fill.toFixed(1).padStart(6)}ms`);
  await b.close();
}

for (const backend of ["cdp-ws", "cdp-pipe", "cdp-raw", "webkit"]) {
  await bench(backend);
}
