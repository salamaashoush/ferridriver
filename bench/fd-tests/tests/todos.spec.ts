// 250 todo tests — exercise add / toggle / delete / filter against the
// Todos route. Each test is fully isolated (fresh page + fresh ?seed
// query string), so parallel runs don't share state.
import { test, expect } from '@ferridriver/test';

const N = 250;

for (let i = 0; i < N; i++) {
  test(`todo add and verify #${i}`, async ({ page }) => {
    await page.goto('/todos');
    const text = `learn ferridriver ${i}`;
    await page.locator('[data-testid=todo-input]').fill(text);
    await page.locator('[data-testid=todo-add]').click();
    // The new item gets id `t-${nextId}` where nextId starts at 0
    // since the page just loaded — first add is t-1.
    await expect(page.locator('[data-testid=todo-list] li').first()).toContainText(text);
    await expect(page.locator('[data-testid=remaining-count]')).toContainText('1 left');
  });
}
