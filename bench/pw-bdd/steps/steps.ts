// Step definitions mirroring ferridriver-bdd's built-in steps
// (crates/ferridriver-bdd/src/steps/*.rs). The grammar is identical so
// the same .feature files run unchanged on both sides.
import { createBdd } from 'playwright-bdd';
import { expect } from '@playwright/test';

const { Given, When, Then } = createBdd();

// ── Navigation ────────────────────────────────────────────────────────
Given('I navigate to {string}', async ({ page }, url: string) => {
  await page.goto(url);
});

// ── Interaction ───────────────────────────────────────────────────────
When('I fill {string} with {string}', async ({ page }, selector: string, value: string) => {
  await page.locator(selector).fill(value);
});

When('I click {string}', async ({ page }, selector: string) => {
  await page.locator(selector).click();
});

When('I select {string} from {string}', async ({ page }, value: string, selector: string) => {
  await page.locator(selector).selectOption(value);
});

When('I check {string}', async ({ page }, selector: string) => {
  await page.locator(selector).check();
});

When('I uncheck {string}', async ({ page }, selector: string) => {
  await page.locator(selector).uncheck();
});

// ── Assertions ────────────────────────────────────────────────────────
Then('{string} should be visible', async ({ page }, selector: string) => {
  await expect(page.locator(selector).first()).toBeVisible();
});

Then('{string} should contain text {string}', async ({ page }, selector: string, expected: string) => {
  await expect(page.locator(selector).first()).toContainText(expected);
});

Then('{string} should have text {string}', async ({ page }, selector: string, expected: string) => {
  await expect(page.locator(selector).first()).toHaveText(expected);
});

Then('{string} should have value {string}', async ({ page }, selector: string, expected: string) => {
  await expect(page.locator(selector).first()).toHaveValue(expected);
});
