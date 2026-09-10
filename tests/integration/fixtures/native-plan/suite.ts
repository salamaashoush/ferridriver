import { test, describe, expect } from '@ferridriver/test';

globalThis.order = [];

test.beforeEach(async () => {
  globalThis.order.push('beforeEach');
});

test('navigates and asserts', async ({ page, browserName }) => {
  globalThis.order.push('body');
  await page.goto('data:text/html,<title>E2E</title><h1 id=h>hello</h1>');
  await expect(page).toHaveTitle('E2E');
  await expect(page.locator('#h')).toHaveText('hello');
  if (typeof browserName !== 'string') throw new Error('browserName missing');
});

describe.serial('steps and info', () => {
  test('step returns value and testInfo works', async ({ page, testInfo }) => {
    const title = await test.step('navigate', async () => {
      await page.goto('data:text/html,<title>Steps</title>');
      return page.title();
    });
    if (title !== 'Steps') throw new Error('step return: ' + title);
    if (!testInfo.title.includes('testInfo works')) throw new Error('title: ' + testInfo.title);
    await testInfo.attach('note', 'text/plain', 'attached', undefined);
    if (testInfo.attachmentCount !== 1) throw new Error('attachmentCount');
  });
});

const extended = test.extend<{ greeting: string }>({
  greeting: async ({ browserName }, use) => {
    await use('hi ' + browserName);
  },
});

extended('custom fixture', async ({ greeting }) => {
  if (!greeting.startsWith('hi ')) throw new Error('greeting: ' + greeting);
});

test('runtime skip is not a failure', async () => {
  test.skip(true, 'demonstrates skip');
  throw new Error('unreachable');
});

test('expected failure inverts', async () => {
  test.fail();
  throw new Error('meant to fail');
});

test('zz hook probe', async () => {
  const order = globalThis.order;
  if (!order.includes('beforeEach')) throw new Error('beforeEach never ran: ' + JSON.stringify(order));
  if (order.indexOf('beforeEach') > order.indexOf('body')) throw new Error('hook after body: ' + JSON.stringify(order));
});

declare global {
  var order: string[];
}
