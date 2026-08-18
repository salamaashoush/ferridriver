// `launch({ proxy })` from JavaScript, for every backend.
//
// The option is declared on `LaunchOptions`, so a caller writing the Playwright
// call has every reason to believe it applies. It has to actually reach the
// browser process: each engine spells the proxy differently, and a switch a
// browser does not recognise is accepted and ignored — a proxy that silently
// does nothing rather than an error.

import { describe, it, expect } from "bun:test";
import { createServer, type Server, type Socket } from "node:net";
import { chromium, firefox, webkit, type Browser } from "../index.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

/** A proxy that answers nothing and records the request lines it is sent. */
function recordingProxy(): Promise<{ port: number; seen: string[]; close: () => void }> {
  const seen: string[] = [];
  const server: Server = createServer((socket: Socket) => {
    socket.once("data", (chunk) => {
      seen.push(chunk.toString().split("\r\n")[0] ?? "");
      socket.destroy();
    });
  });

  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      resolve({ port, seen, close: () => server.close() });
    });
  });
}

function launchWithProxy(backend: string, port: number): Promise<Browser> {
  const proxy = { server: `http://127.0.0.1:${port}`, bypass: "127.0.0.1,localhost" };
  switch (backend) {
    case "cdp-pipe":
      return chromium().launch({ headless: true, proxy });
    case "cdp-raw":
      return chromium({ transport: "ws" }).launch({ headless: true, proxy });
    case "bidi":
      return firefox().launch({ headless: true, proxy });
    case "webkit":
      return webkit().launch({ headless: true, proxy });
    default:
      throw new Error(`Unknown backend: ${backend}`);
  }
}

for (const backend of BACKENDS) {
  describe(`launch({ proxy }) [${backend}]`, () => {
    it("routes page traffic through the proxy", async () => {
      const proxy = await recordingProxy();
      const browser = await launchWithProxy(backend, proxy.port);

      try {
        const page = await browser.newPage();
        // Nothing resolves this name, so only the proxy could have been asked.
        await page.goto("https://proxy-probe.invalid/", { timeout: 5000 }).catch(() => {});

        expect(proxy.seen.some((line) => line.includes("proxy-probe.invalid"))).toBe(true);
      } finally {
        await browser.close();
        proxy.close();
      }
    }, 60_000);
  });
}
