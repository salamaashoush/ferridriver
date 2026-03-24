import { Browser } from "../index.js";
const b = await Browser.launch({ backend: "cdp-pipe" });
const p = await b.newPageWithUrl("https://example.com");
// Force engine inject
await p.evaluate("1");
await p.innerText("h1").catch(e => console.log("1st:", e.message));
await p.innerText("h1").catch(e => console.log("2nd:", e.message));
// Try with simple CSS selector via page.click (which uses find_element)
try {
  const text = await p.innerText("h1");
  console.log("text:", text);
} catch(e: any) {
  console.log("3rd:", e.message);
}
// Use locator with forced sync
const loc = p.locator("h1");
const count = await loc.count();
console.log("count:", count);
await b.close();
