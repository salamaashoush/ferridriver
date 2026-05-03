// 250 blog tests — list+search+navigate+detail. Walks the React-Query
// async path and react-router param routing.
import { test, expect } from '@playwright/test';

const N_LIST = 50;
const N_DETAIL = 200;

for (let i = 0; i < N_LIST; i++) {
  // Search by tag — exercises the data-fetch + client-side filter path.
  const tag = ['rust', 'typescript', 'react', 'cdp', 'perf', 'web', 'ai', 'api'][i % 8];
  test(`blog search by tag ${tag} #${i}`, async ({ page }) => {
    await page.goto('/blog');
    await expect(page.locator('[data-testid=blog-list] li').first()).toBeVisible();
    await page.locator('[data-testid=blog-search]').fill(tag);
    await expect(page.locator('[data-testid=blog-count]')).toContainText('matches');
    await expect(page.locator('[data-testid=blog-list] li').first()).toBeVisible();
  });
}

for (let i = 0; i < N_DETAIL; i++) {
  // Visit specific post detail — walks router params + nested fetch.
  const slug = `post-${String(i).padStart(3, '0')}`;
  test(`blog detail ${slug}`, async ({ page }) => {
    await page.goto(`/blog/${slug}`);
    await expect(page.locator('[data-testid=post-title]')).toContainText(`Post ${i}:`);
    await expect(page.locator('[data-testid=post-body]')).toContainText('lorem ipsum');
    await page.locator('[data-testid=back-link]').click();
    await expect(page.locator('[data-testid=blog-title]')).toBeVisible();
  });
}
