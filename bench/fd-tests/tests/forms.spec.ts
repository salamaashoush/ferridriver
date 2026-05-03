// 200 form tests — validation paths + successful submit + async submit.
import { test, expect } from '@ferridriver/test';

const ROLES = ['user', 'admin', 'guest'] as const;

// 100 successful submits with varying fields.
for (let i = 0; i < 100; i++) {
  test(`form submit valid #${i}`, async ({ page }) => {
    await page.goto('/forms');
    await page.locator('[data-testid=form-name]').fill(`Tester ${i}`);
    await page.locator('[data-testid=form-email]').fill(`tester${i}@example.com`);
    await page.locator('[data-testid=form-age]').fill(String(20 + (i % 50)));
    await page.locator('[data-testid=form-role]').selectOption(ROLES[i % 3]);
    await page.locator('[data-testid=form-bio]').fill(`bio for tester ${i}`);
    await page.locator('[data-testid=form-agree]').check();
    await page.locator('[data-testid=form-submit]').click();
    await expect(page.locator('[data-testid=submit-result]')).toBeVisible();
    await expect(page.locator('[data-testid=submit-payload]')).toContainText(`Tester ${i}`);
  });
}

// 100 invalid-field tests — different error per test.
for (let i = 0; i < 100; i++) {
  const variants = [
    { field: 'form-name', value: 'x', error: 'error-name', message: 'at least 2' },
    { field: 'form-email', value: 'not-an-email', error: 'error-email', message: 'invalid' },
    { field: 'form-age', value: '5', error: 'error-age', message: '13 or older' },
    { field: 'form-bio', value: 'a'.repeat(300), error: 'error-bio', message: '280' },
  ];
  const v = variants[i % variants.length];
  test(`form validation #${i} (${v.field})`, async ({ page }) => {
    await page.goto('/forms');
    // Fill everything else valid; only the targeted field is bad.
    await page.locator('[data-testid=form-name]').fill('Valid Name');
    await page.locator('[data-testid=form-email]').fill('valid@example.com');
    await page.locator('[data-testid=form-age]').fill('30');
    await page.locator('[data-testid=form-bio]').fill('valid bio');
    await page.locator('[data-testid=form-agree]').check();
    await page.locator(`[data-testid=${v.field}]`).fill(v.value);
    await page.locator('[data-testid=form-submit]').click();
    await expect(page.locator(`[data-testid=${v.error}]`)).toContainText(v.message);
  });
}
