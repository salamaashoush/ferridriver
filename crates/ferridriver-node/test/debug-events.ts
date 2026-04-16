import { Browser } from "../index.js";
const b = await Browser.launch({ backend: "cdp-pipe" });
const p = await b.newPage();

p.on("console", (...args: any[]) => {
  console.log("CALLBACK ARGS:", JSON.stringify(args));
  console.log("CALLBACK ARG0:", typeof args[0], args[0]);
});

await p.setContent('<script>console.log("test message")</script>');
await new Promise(r => setTimeout(r, 1000));
await b.close();
