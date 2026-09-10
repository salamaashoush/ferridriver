import { test, expect } from '@ferridriver/test';

test('evaluation preserves special numbers in arguments and results', async ({ page }) => {
  for (const value of [NaN, Infinity, -Infinity, -0, 0]) {
    const returned = await page.evaluate((value: number) => value, value);
    expect(Object.is(returned, value)).toBe(true);
    const nested = await page.evaluate(({ value }: { value: number }) => ({ value }), { value }) as { value: number };
    expect(Object.is(nested.value, value)).toBe(true);
  }
});
