// `browserType.launchServer()` from Node: the endpoint it hands out is a
// DevTools WebSocket, or Firefox's WebDriver BiDi, another connection
// drives, admitted only with its token.
import { describe, it, expect } from "bun:test";
import { chromium, firefox, webkit } from "../index.js";

describe("launchServer", () => {
  it("serves chromium to a second connection that brings the token", async () => {
    const server = await chromium().launchServer({ headless: true, token: "sashoush-node-token" });
    try {
      const endpoint = server.wsEndpoint();
      expect(endpoint).toMatch(/^ws:\/\/127\.0\.0\.1:\d+\/[0-9a-f]{64}\?token=sashoush-node-token$/);

      const refused = await chromium()
        .connectOverCDP(endpoint.replace("sashoush-node-token", "wrong"), { timeout: 5000 })
        .then(() => "connected", (error: Error) => error.message);
      expect(refused).not.toBe("connected");

      const browser = await chromium().connectOverCDP(endpoint);
      try {
        const context = await browser.newContext();
        const page = await context.newPage();
        await page.setContent("<title>remote</title>");
        expect(await page.title()).toBe("remote");
      } finally {
        await browser.close();
      }
    } finally {
      await server.close();
    }
  }, 60_000);

  it("serves firefox over BiDi to a second connection", async () => {
    const server = await firefox().launchServer({ headless: true, token: "sashoush-node-token" });
    try {
      const browser = await firefox().connect(server.wsEndpoint());
      try {
        const page = await (await browser.newContext()).newPage();
        await page.setContent("<title>remote</title>");
        expect(await page.title()).toBe("remote");
      } finally {
        await browser.close();
      }
    } finally {
      await server.close();
    }
  }, 60_000);

  it("refuses webkit, which serves neither protocol", async () => {
    const refused = await webkit()
      .launchServer()
      .then(() => "served", (error: Error) => error.message);
    expect(refused).toContain("WebDriver BiDi");
  });
});
