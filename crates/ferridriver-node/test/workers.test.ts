// page.workers(), the 'worker' event and Worker.evaluate through the
// Node binding.
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", ...(process.platform === "darwin" ? ["webkit"] : [])];

const START = `window.__worker = new Worker(URL.createObjectURL(new Blob(['self.answer = 42; onmessage = () => {};'], { type: 'application/javascript' })))`;

for (const backend of BACKENDS) {
  describe(`[${backend}] workers`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    });

    afterAll(async () => {
      await browser.close();
    });

    it("reports a worker, evaluates in it, and closes it", async () => {
      await page.setContent("<p>workers</p>");
      expect(page.workers()).toEqual([]);
      const [worker] = (await Promise.all([page.waitForEvent("worker"), page.evaluate(START)])) as any[];
      expect(worker.url()).toContain("blob:");
      expect(page.workers()[0]).toBe(worker);
      let answer: unknown;
      for (let i = 0; i < 50 && answer !== 42; i++) answer = await worker.evaluate("self.answer");
      expect(answer).toBe(42);
      expect(await worker.evaluate((n: number) => n * 2, 21)).toBe(42);
      await Promise.all([worker.waitForEvent("close", { timeout: 5000 }), page.evaluate("window.__worker.terminate()")]);
      expect(page.workers()).toEqual([]);
      const after = await worker.evaluate("1").then(() => "resolved", (e: Error) => e.message);
      expect(after).toContain("closed");
    });

    it("close listeners hear the worker until removed", async () => {
      await page.setContent("<p>workers</p>");
      const [worker] = (await Promise.all([page.waitForEvent("worker"), page.evaluate(START)])) as any[];
      const heard: string[] = [];
      const removed = () => heard.push("removed");
      worker.on("close", (closed: unknown) => heard.push(closed === worker ? "same" : "other"));
      worker.once("close", () => heard.push("once"));
      worker.on("close", removed);
      worker.off("close", removed);
      await Promise.all([worker.waitForEvent("close", { timeout: 5000 }), page.evaluate("window.__worker.terminate()")]);
      for (let i = 0; i < 50 && heard.length < 2; i++) await Bun.sleep(20);
      expect(heard).toEqual(["same", "once"]);
    });
  });
}
