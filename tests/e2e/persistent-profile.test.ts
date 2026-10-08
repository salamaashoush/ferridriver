import { test, expect } from '@ferridriver/test';
import { readdir } from 'node:fs/promises';

test('a persistent profile keeps its localStorage after the browser closes', async ({ browserName, baseURL }, info) => {
  // Both closes are graceful: the browser writes the profile out before it
  // exits. Under the full suite on macOS that exit alone measured 12-16s
  // per close, against well under a second when the test runs by itself.
  test.slow();
  const factory = browserName === 'firefox' ? firefox() : browserName === 'webkit' ? webkit() : chromium();
  const profile = info.outputPath('profile');
  const value = `sashoush-${crypto.randomUUID()}`;
  let context = await factory.launchPersistentContext(profile, { headless: true });
  try {
    const page = await context.newPage();
    await page.goto(`${baseURL}/fx/landed`);
    await page.evaluate((value: string) => localStorage.setItem('profile-owner', value), value);
  } finally {
    await context.close();
  }
  expect((await readdir(profile)).length).toBeGreaterThan(0);
  context = await factory.launchPersistentContext(profile, { headless: true });
  try {
    const page = await context.newPage();
    await page.goto(`${baseURL}/fx/landed`);
    expect(await page.evaluate(() => localStorage.getItem('profile-owner'))).toBe(value);
  } finally {
    await context.close();
  }
  expect((await readdir(profile)).length).toBeGreaterThan(0);
});
