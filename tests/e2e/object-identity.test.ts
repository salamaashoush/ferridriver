// One object per page and per frame, as Playwright's client gives:
// the frame a call returns is the frame another call returned, and the
// main frame leads page.frames().
import { test, expect } from '@ferridriver/test';
import type { Frame } from '@ferridriver/test';

test('pages and frames keep their identity', async ({ page, context }) => {
  await page.setContent('<iframe srcdoc="<p>child</p>"></iframe>');
  await expect.poll(() => page.frames().length).toBe(2);
  const main = page.mainFrame();
  const [first, child] = page.frames();
  expect(main === page.mainFrame()).toBe(true);
  expect(first === main).toBe(true);
  expect(child.parentFrame() === main).toBe(true);
  expect(main.childFrames()[0] === child).toBe(true);
  expect(main.page() === page).toBe(true);
  expect(child.page() === page).toBe(true);
  expect(page.locator('iframe').page() === page).toBe(true);
  expect((await context.pages()).includes(page)).toBe(true);
  const iframe = await page.locator('iframe').elementHandle();
  expect((await iframe.contentFrame()) === child).toBe(true);
});

test('a frame event hands over the frame page.frames() knows', async ({ page }) => {
  await page.setContent('<p>main</p>');
  const attached = page.waitForEvent('frameattached');
  await page.evaluate(() => {
    const iframe = document.createElement('iframe');
    iframe.srcdoc = '<p>late</p>';
    document.body.appendChild(iframe);
  });
  const frame = await attached as Frame;
  expect(page.frames().includes(frame)).toBe(true);
});
