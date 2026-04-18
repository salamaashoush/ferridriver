import { Browser } from "../index.js";
const b = await Browser.launch({ backend: "cdp-pipe" });
const p = await b.newPageWithUrl("about:blank");
try {
  await p.setLocale("de-DE");
  console.log("setLocale succeeded");
} catch(e: any) {
  console.log("setLocale error:", e.message);
}
// Try the Playwright approach: set via Network.setUserAgentOverride with acceptLanguage
const lang = await p.evaluate("navigator.language");
console.log("lang:", lang);
await b.close();
