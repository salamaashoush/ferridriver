// 100 multi-step wizard tests — exercise multi-page state retention.
import { test, expect } from '@ferridriver/test';

for (let i = 0; i < 100; i++) {
  test(`wizard end-to-end #${i}`, async ({ page }) => {
    await page.goto('/wizard');

    // Step 0: account
    await page.locator('[data-testid=wiz-username]').fill(`user${i}`);
    await page.locator('[data-testid=wiz-password]').fill(`secret${i}`);
    await page.locator('[data-testid=wiz-next]').click();

    // Step 1: profile
    await expect(page.locator('[data-testid=step-1]')).toHaveAttribute('data-current', 'true');
    await page.locator('[data-testid=wiz-display]').fill(`Display ${i}`);
    await page.locator('[data-testid=wiz-tagline]').fill(`Tagline ${i}`);
    await page.locator('[data-testid=wiz-next]').click();

    // Step 2: preferences
    await expect(page.locator('[data-testid=step-2]')).toHaveAttribute('data-current', 'true');
    if (i % 2 === 0) {
      await page.locator('[data-testid=wiz-theme]').selectOption('light');
    }
    if (i % 3 === 0) {
      await page.locator('[data-testid=wiz-notifications]').uncheck();
    }
    await page.locator('[data-testid=wiz-next]').click();

    // Step 3: review
    await expect(page.locator('[data-testid=review-username]')).toContainText(`user${i}`);
    await expect(page.locator('[data-testid=review-display]')).toContainText(`Display ${i}`);
    await expect(page.locator('[data-testid=review-tagline]')).toContainText(`Tagline ${i}`);
    await expect(page.locator('[data-testid=review-theme]')).toContainText(
      i % 2 === 0 ? 'light' : 'dark',
    );
    await expect(page.locator('[data-testid=review-notif]')).toContainText(
      i % 3 === 0 ? 'off' : 'on',
    );
  });
}
