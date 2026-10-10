/**
 * Locator.toString(): the JavaScript spelling Playwright's
 * asLocatorDescription gives the locator's selector.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] Locator.toString`, () => {
  let browser: Browser;
  let page: Page;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
    page = await browser.newPage();
  });

  afterAll(async () => {
    await browser.close();
  });

  it("prints the locator as code", () => {
    expect(String(page.getByRole("button", { name: "Submit" }))).toBe(`getByRole('button', { name: 'Submit' })`);
    expect(page.getByText("Hello", { exact: true }).toString()).toBe(`getByText('Hello', { exact: true })`);
    expect(String(page.locator("div").filter({ hasText: "foo" }).nth(2))).toBe(`locator('div').filter({ hasText: 'foo' }).nth(2)`);
    expect(String(page.getByTestId(/He"llo/))).toBe(`getByTestId(/He"llo/)`);
  });

  it("prints the description describe() set", () => {
    expect(String(page.locator("#submit").describe("Submit button"))).toBe("Submit button");
  });
});
