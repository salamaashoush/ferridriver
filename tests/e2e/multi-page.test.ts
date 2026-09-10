import { test, expect } from '@ferridriver/test';

test('independent pages keep their input, actions, and screenshots separate', async ({ context }) => {
  const pages = await Promise.all([context.newPage(), context.newPage(), context.newPage()]);
  try {
    const [input, button, list] = pages;
    await Promise.all([
      input.setContent("<h1>Page One</h1><input id='i' type='text'>"),
      button.setContent(`<h1>Page Two</h1><button id='b' onclick="this.textContent='clicked'">Go</button>`),
      list.setContent('<h1>Page Three</h1><ul><li>A</li><li>B</li><li>C</li></ul>'),
    ]);
    await Promise.all([input.locator('#i').fill('multi-page'), button.locator('#b').click()]);
    await expect(input.locator('#i')).toHaveValue('multi-page');
    await expect(button.locator('#b')).toHaveText('clicked');
    await expect(list.locator('li')).toHaveCount(3);
    const [first, second] = await Promise.all([input.screenshot(), button.screenshot()]);
    expect(first.length).toBeGreaterThan(100);
    expect(second.length).toBeGreaterThan(100);
    expect(first).not.toEqual(second);
  } finally {
    await Promise.all(pages.map(page => page.close()));
  }
});
