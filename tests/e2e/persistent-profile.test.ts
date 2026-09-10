import { test, expect } from '@ferridriver/test';
import { readdir } from 'node:fs/promises';

test('a persistent profile keeps its localStorage after the browser closes', async ({ browserName, baseURL }, info) => {
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
