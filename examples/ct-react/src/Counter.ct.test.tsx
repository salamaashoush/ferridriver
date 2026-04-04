/**
 * React Counter component test.
 *
 * Uses the unified @ferridriver/test CLI with --ct flag:
 *   cd examples/ct-react
 *   ferridriver-test --ct --framework react src/Counter.ct.test.tsx
 *
 * The CLI handles: import transform → Vite build → serve → mount() fixture.
 */

import { test, expect } from '@ferridriver/test';
import Counter from './Counter.tsx';

test('mounts and renders initial count', async ({ mount, page }) => {
  await mount(Counter, { props: { initial: 0 } });
  await expect(page.locator('#count')).toHaveText('0');
});

test('increments on + click', async ({ mount, page }) => {
  await mount(Counter, { props: { initial: 0 } });
  await page.locator('#inc').click();
  await expect(page.locator('#count')).toHaveText('1');
});

test('decrements on - click', async ({ mount, page }) => {
  await mount(Counter, { props: { initial: 5 } });
  await page.locator('#dec').click();
  await expect(page.locator('#count')).toHaveText('4');
});
