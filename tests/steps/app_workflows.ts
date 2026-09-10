import { expect } from '@ferridriver/test';

Given('Salama has filled the required registration details', async ({ page }) => {
  await page.goto('/forms.html');
  await page.getByLabel('Full Name', { exact: true }).fill('Salama Ashoush');
  await page.getByRole('textbox', { name: 'Email', exact: true }).fill('salama@example.com');
  await page.getByLabel('Password', { exact: true }).fill('browser-test-password');
  await page.getByLabel('Confirm Password', { exact: true }).fill('browser-test-password');
  await page.getByLabel('Country', { exact: true }).selectOption('de');
  await page.getByLabel('I agree to the Terms of Service').check();
});

When('I submit the registration', async ({ page }) => {
  await page.getByRole('button', { name: 'Register', exact: true }).click();
});

Then('the registration receipt contains:', async ({ page }, table: DataTable) => {
  const receipt = page.locator('#output');
  await expect(receipt).not.toBeEmpty();
  expect(JSON.parse((await receipt.textContent())!)).toMatchObject(table.rowsHash());
});

Then('the registration does not subscribe to the newsletter', async ({ page }) => {
  const receipt = JSON.parse((await page.locator('#output').textContent())!);
  expect(Object.hasOwn(receipt, 'newsletter')).toBe(false);
});

Then('the registration is blocked by {string}', async ({ page }, field: string) => {
  await expect(page.locator(`${field}:invalid`)).toHaveCount(1);
  await expect(page.locator('#output')).toBeEmpty();
});

Given('my task list contains:', async ({ page }, table: DataTable) => {
  await page.goto('https://demo.playwright.dev/todomvc/#/');
  for (const [title] of table.raw()) {
    await page.getByPlaceholder('What needs to be done?').fill(title);
    await page.getByPlaceholder('What needs to be done?').press('Enter');
  }
  await expect(page.locator('.todo-list li label')).toHaveText(table.raw().map(([title]) => title));
});

Then('the task list shows in order:', async ({ page }, table: DataTable) => {
  await expect(page.locator('.todo-list li label')).toHaveText(table.raw().map(([title]) => title));
});

Then('the task list has {int} remaining tasks', async ({ page }, count: number) => {
  await expect(page.locator('.todo-count')).toHaveText(`${count} ${count === 1 ? 'item' : 'items'} left`);
});
