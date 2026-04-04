/**
 * TodoMVC component tests — same 15 tests as the Rust framework examples.
 *
 * Run: ferridriver-test --ct --framework react src/todomvc.ct.ts
 */
import { test, expect } from '@ferridriver/test';

async function addTodo(page: any, text: string) {
  await page.locator('#new-todo').click();
  await page.locator('#new-todo').typeText(text);
  await page.locator('#new-todo').press('Enter');
}

// ── Adding ──

test('add single todo', async ({ page }) => {
  await addTodo(page, 'Buy milk');
  await expect(page.locator('.todo-list li')).toHaveCount(1);
  await expect(page.locator('.todo-list li label')).toHaveText('Buy milk');
});

test('add multiple todos', async ({ page }) => {
  await addTodo(page, 'Buy milk');
  await addTodo(page, 'Walk the dog');
  await addTodo(page, 'Write tests');
  await expect(page.locator('.todo-list li')).toHaveCount(3);
});

test('empty input does not add', async ({ page }) => {
  await page.locator('#new-todo').press('Enter');
  await expect(page.locator('.todo-list li')).toHaveCount(0);
});

test('input clears after add', async ({ page }) => {
  await addTodo(page, 'Test');
  await expect(page.locator('#new-todo')).toHaveValue('');
});

// ── Count ──

test('shows item count', async ({ page }) => {
  await addTodo(page, 'One');
  await expect(page.locator('#todo-count')).toHaveText('1 item left');
  await addTodo(page, 'Two');
  await expect(page.locator('#todo-count')).toHaveText('2 items left');
});

// ── Complete ──

test('toggle todo complete', async ({ page }) => {
  await addTodo(page, 'Buy milk');
  await page.locator('.todo-list li:nth-child(1) .toggle').click();
  await expect(page.locator('.todo-list li.completed')).toHaveCount(1);
  await expect(page.locator('#todo-count')).toHaveText('0 items left');
});

test('toggle todo uncomplete', async ({ page }) => {
  await addTodo(page, 'Buy milk');
  await page.locator('.todo-list li:nth-child(1) .toggle').click();
  await page.locator('.todo-list li:nth-child(1) .toggle').click();
  await expect(page.locator('#todo-count')).toHaveText('1 item left');
});

// ── Delete ──

test('delete todo', async ({ page }) => {
  await addTodo(page, 'Delete me');
  await addTodo(page, 'Keep me');
  await page.locator('.todo-list li:nth-child(1) .destroy').click();
  await expect(page.locator('.todo-list li')).toHaveCount(1);
  await expect(page.locator('.todo-list li label')).toHaveText('Keep me');
});

// ── Filter ──

test('filter active', async ({ page }) => {
  await addTodo(page, 'Active todo');
  await addTodo(page, 'Completed todo');
  await page.locator('.todo-list li:nth-child(2) .toggle').click();
  await page.locator('#filter-active').click();
  await expect(page.locator('.todo-list li')).toHaveCount(1);
  await expect(page.locator('.todo-list li label')).toHaveText('Active todo');
});

test('filter completed', async ({ page }) => {
  await addTodo(page, 'Active todo');
  await addTodo(page, 'Completed todo');
  await page.locator('.todo-list li:nth-child(2) .toggle').click();
  await page.locator('#filter-completed').click();
  await expect(page.locator('.todo-list li')).toHaveCount(1);
  await expect(page.locator('.todo-list li label')).toHaveText('Completed todo');
});

test('filter all shows everything', async ({ page }) => {
  await addTodo(page, 'One');
  await addTodo(page, 'Two');
  await page.locator('.todo-list li:nth-child(1) .toggle').click();
  await page.locator('#filter-active').click();
  await expect(page.locator('.todo-list li')).toHaveCount(1);
  await page.locator('#filter-all').click();
  await expect(page.locator('.todo-list li')).toHaveCount(2);
});

// ── Clear completed ──

test('clear completed', async ({ page }) => {
  await addTodo(page, 'Keep');
  await addTodo(page, 'Remove');
  await addTodo(page, 'Also remove');
  await page.locator('.todo-list li:nth-child(2) .toggle').click();
  await page.locator('.todo-list li:nth-child(3) .toggle').click();
  await page.locator('#clear-completed').click();
  await expect(page.locator('.todo-list li')).toHaveCount(1);
  await expect(page.locator('.todo-list li label')).toHaveText('Keep');
});

// ── Toggle all ──

test('toggle all completes all', async ({ page }) => {
  await addTodo(page, 'One');
  await addTodo(page, 'Two');
  await addTodo(page, 'Three');
  await page.locator('#toggle-all').click();
  await expect(page.locator('#todo-count')).toHaveText('0 items left');
});

test('toggle all uncompletes when all done', async ({ page }) => {
  await addTodo(page, 'One');
  await addTodo(page, 'Two');
  await page.locator('#toggle-all').click();
  await page.locator('#toggle-all').click();
  await expect(page.locator('#todo-count')).toHaveText('2 items left');
});

// ── Edit ──

test('edit todo on double click', async ({ page }) => {
  await addTodo(page, 'Original text');
  await page.locator('.todo-list li:nth-child(1) label').dblclick();
  await expect(page.locator('.edit-input')).toBeVisible();
  // React controlled inputs need typeText (fill() doesn't dispatch React-compatible events yet).
  await page.locator('.edit-input').clear();
  await page.locator('.edit-input').typeText('Updated text');
  await page.locator('.edit-input').press('Enter');
  await expect(page.locator('.todo-list li:nth-child(1) label')).toHaveText('Updated text');
});
