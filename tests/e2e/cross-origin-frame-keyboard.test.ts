// Typing reaches an input inside a cross-origin iframe: fill and press
// focus it first, and the keys go to the frame that holds the focus, not
// to whatever the page had focused before.

import { describe, expect, test } from '@ferridriver/test';

describe('keyboard input in a cross-origin iframe', () => {
  test('fill and press reach the input, and the page button keeps its keys', async ({ page }) => {
    await page.goto('http://127.0.0.1:47831/fx/landed');
    await page.evaluate(
      (src: string) =>
        new Promise((resolve) => {
          document.body.innerHTML = '<button id="open">Open</button>';
          const button = document.getElementById('open')!;
          (window as any).presses = 0;
          button.addEventListener('click', () => (window as any).presses++);
          button.focus();
          const frame = document.createElement('iframe');
          frame.name = 'studio';
          frame.src = src;
          frame.onload = resolve;
          document.body.appendChild(frame);
        }),
      'http://localhost:47831/fx/landed',
    );
    const studio = page.frameLocator('iframe[name="studio"]');
    await studio.locator('body').evaluate((body: HTMLElement) => {
      body.innerHTML = '<input id="name" value="Sprite1">';
      (window as any).entered = 0;
      document.getElementById('name')!.addEventListener('keydown', (e) => {
        if ((e as KeyboardEvent).key === 'Enter') (window as any).entered++;
      });
    });
    const name = studio.locator('#name');
    await name.fill('Hero');
    await expect(name).toHaveValue('Hero');
    await name.press('Enter');
    expect(await name.evaluate(() => (window as any).entered)).toBe(1);
    expect(await page.evaluate(() => (window as any).presses)).toBe(0);
  });
});
