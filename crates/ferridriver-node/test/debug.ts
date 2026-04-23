import { chromium } from "../index.js";
const b = await chromium().launch();
const ctx = b.newContext({ locale: "de-DE" });
const p = await ctx.newPageWithUrl("about:blank");
const lang = await p.evaluate("navigator.language");
console.log("lang:", lang);
await b.close();
