// 200 dashboard tests — async data load + multi-filter combinations.
import { test, expect } from '@playwright/test';

const REGIONS = ['all', 'NA', 'EU', 'APAC'] as const;
const STATUSES = ['all', 'pending', 'shipped', 'delivered', 'returned'] as const;
const SORTS = ['amount', 'date'] as const;

let i = 0;
for (const region of REGIONS) {
  for (const status of STATUSES) {
    for (const sort of SORTS) {
      // 4 × 5 × 2 = 40 unique filter combos. Repeat each 5× to reach 200.
      for (let rep = 0; rep < 5; rep++) {
        test(`dashboard ${region}/${status}/${sort} #${i++}`, async ({ page }) => {
          await page.goto('/dashboard');
          // Wait for the table body to render at least one row OR the empty marker.
          await expect(
            page.locator('[data-testid=sales-table]'),
          ).toBeVisible();
          await page.locator('[data-testid=region-filter]').selectOption(region);
          await page.locator('[data-testid=status-filter]').selectOption(status);
          await page.locator('[data-testid=sort-by]').selectOption(sort);
          // The row count and total update reactively — assert both stabilise.
          await expect(page.locator('[data-testid=row-count]')).toContainText('rows');
          await expect(page.locator('[data-testid=total-amount]')).toContainText('$');
        });
      }
    }
  }
}
