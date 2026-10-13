// Mobile Safari in an iOS simulator, driven over Appium's Classic WebDriver.
// These are the places where that route differs from a desktop browser's
// own protocol: how a tab is opened, how a data: URL is reached, how a
// window the page opened is found, and which cookies a session can see.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

test.describe('mobile Safari', () => {
  test('goes back to the first page a tab visited', async ({ page, baseURL }) => {
    const first = `${baseURL}/fx/srcdoc`;
    const second = `${baseURL}/fx/landed`;
    await page.goto(first);
    await page.goto(second);
    await page.goBack();
    expect(page.url()).toBe(first);
    expect(await page.evaluate(() => location.href)).toBe(first);
    expect(page.frames().length).toBe(2);
    await page.goForward();
    expect(page.url()).toBe(second);
    await page.goBack();
    await page.goBack();
    expect(page.url()).toBe('about:blank');
  });

  test('reaches a data: URL that the address field would search for', async ({ page }) => {
    await page.goto('data:text/html,<title>Two words</title><p>one two</p>');
    expect(await page.title()).toBe('Two words');
    expect(await page.locator('p').textContent()).toBe('one two');
    await page.goto('data:text/html,<title>Next page</title>');
    expect(await page.title()).toBe('Next page');
  });

  test('reports the tab a link opened as a popup of the page', async ({ page, context, baseURL }) => {
    const target = `${baseURL}/fx/landed?popup=1`;
    await page.goto(`${baseURL}/fx/landed`);
    await page.setContent(`<a target="_blank" rel="opener" href="${target}" style="font-size:40px">open</a>`);
    const [popup] = await Promise.all([page.waitForEvent('popup') as Promise<Page>, page.click('a')]);
    await expect.poll(() => popup.evaluate(() => location.href)).toBe(target);
    expect(await popup.evaluate(() => !!window.opener)).toBe(true);
    expect(await popup.opener()).toBe(page);
    expect(context.pages().length).toBe(2);
    await popup.close();
    expect(await page.evaluate(() => document.querySelector('a')!.textContent)).toBe('open');
  });

  test('refuses a cookie for a host the document is not on', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await context.addCookies([{ name: 'here', value: '1', url: baseURL! }]);
    expect((await context.cookies()).map(c => c.name)).toEqual(['here']);
    const refused = await context
      .addCookies([{ name: 'there', value: '1', domain: 'localhost', path: '/' }])
      .then(() => 'accepted', (e: Error) => e.message);
    expect(refused).toContain('cannot hold one for "localhost"');
  });

  test('starts a test without the cookies of the one before', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    expect(await context.cookies()).toEqual([]);
    expect(await page.evaluate(() => document.cookie)).toBe('');
  });
});
