// Ported from Playwright's tests/library/locator-generator.spec.ts: the
// JavaScript spelling each locator prints as, built through the real
// locator API so the selector each method encodes is checked with it.
import { test, expect } from '@ferridriver/test';

test.describe('locator.toString()', () => {
  test('reverse engineers the getBy locators', async ({ page }) => {
    expect(String(page.getByTestId('Hello'))).toBe("getByTestId('Hello')");
    expect(String(page.getByTestId('He"llo'))).toBe('getByTestId(\'He"llo\')');
    expect(String(page.getByTestId(/He"llo/))).toBe('getByTestId(/He"llo/)');
    expect(String(page.getByTestId(/He\\"llo/))).toBe('getByTestId(/He\\\\"llo/)');
    expect(String(page.getByText('Hello', { exact: true }))).toBe('getByText(\'Hello\', { exact: true })');
    expect(String(page.getByText('Hello'))).toBe('getByText(\'Hello\')');
    expect(String(page.getByText(/Hello/))).toBe('getByText(/Hello/)');
    expect(String(page.getByLabel('Name'))).toBe('getByLabel(\'Name\')');
    expect(String(page.getByLabel('Last Name', { exact: true }))).toBe('getByLabel(\'Last Name\', { exact: true })');
    expect(String(page.getByLabel(/Last\s+name/i))).toBe('getByLabel(/Last\\s+name/i)');
    expect(String(page.getByPlaceholder('hello'))).toBe('getByPlaceholder(\'hello\')');
    expect(String(page.getByPlaceholder('Hello', { exact: true }))).toBe('getByPlaceholder(\'Hello\', { exact: true })');
    expect(String(page.getByPlaceholder(/wor/i))).toBe('getByPlaceholder(/wor/i)');
    expect(String(page.getByAltText('hello'))).toBe('getByAltText(\'hello\')');
    expect(String(page.getByAltText('Hello', { exact: true }))).toBe('getByAltText(\'Hello\', { exact: true })');
    expect(String(page.getByAltText(/wor/i))).toBe('getByAltText(/wor/i)');
    expect(String(page.getByTitle('hello'))).toBe('getByTitle(\'hello\')');
    expect(String(page.getByTitle('Hello', { exact: true }))).toBe('getByTitle(\'Hello\', { exact: true })');
    expect(String(page.getByTitle(/wor/i))).toBe('getByTitle(/wor/i)');
    expect(String(page.getByPlaceholder('hello my\nwo"rld'))).toBe('getByPlaceholder(\'hello my\\nwo"rld\')');
    expect(String(page.getByAltText('hello my\nwo"rld'))).toBe('getByAltText(\'hello my\\nwo"rld\')');
    expect(String(page.getByTitle('hello my\nwo"rld'))).toBe('getByTitle(\'hello my\\nwo"rld\')');
    expect(String(page.getByText('hello my\nwo"rld'))).toBe('getByText(\'hello my\\nwo"rld\')');
    expect(String(page.getByText('hello       my     wo"rld'))).toBe('getByText(\'hello       my     wo"rld\')');
    expect(String(page.getByLabel('hello my\nwo"rld'))).toBe('getByLabel(\'hello my\\nwo"rld\')');
  });

  test('reverse engineers getByRole', async ({ page }) => {
    expect(String(page.getByRole('button'))).toBe(`getByRole('button')`);
    expect(String(page.getByRole('heading', {}))).toBe("getByRole('heading')");
    expect(String(page.getByRole('button', { name: 'Hello' }))).toBe(`getByRole('button', { name: 'Hello' })`);
    expect(String(page.getByRole('button', { name: /Hello/ }))).toBe(`getByRole('button', { name: /Hello/ })`);
    expect(String(page.getByRole('button', { name: 'He"llo', exact: true }))).toBe(`getByRole('button', { name: 'He"llo', exact: true })`);
    expect(String(page.getByRole('button', { checked: true, pressed: false, level: 3 }))).toBe(`getByRole('button', { checked: true, level: 3, pressed: false })`);
    expect(String(page.getByRole('alert', { name: 'Upload', description: 'doc.pdf' }))).toBe(`getByRole('alert', { name: 'Upload', description: 'doc.pdf' })`);
    expect(String(page.getByRole('alert', { description: 'doc.pdf' }))).toBe(`getByRole('alert', { description: 'doc.pdf' })`);
    expect(String(page.getByRole('alert', { description: /doc\.pdf/ }))).toBe(`getByRole('alert', { description: /doc\\.pdf/ })`);
    expect(String(page.getByRole('alert', { name: 'Upload', description: 'doc.pdf', exact: true }))).toBe(`getByRole('alert', { name: 'Upload', description: 'doc.pdf', exact: true })`);
  });

  test('reverse engineers ordering, regexes and filters', async ({ page }) => {
    expect(String(page.locator('div').nth(3).first().last())).toBe(`locator('div').nth(3).first().last()`);
    expect(String(page.getByText(/he\/\sl\nlo/))).toBe(`getByText(/he\\/\\sl\\nlo/)`);
    expect(String(page.getByPlaceholder(/he\/\sl\nlo/))).toBe(`getByPlaceholder(/he\\/\\sl\\nlo/)`);
    expect(String(page.getByText(/hel"lo/))).toBe(`getByText(/hel\"lo/)`);
    expect(String(page.getByPlaceholder(/hel"lo/))).toBe(`getByPlaceholder(/hel"lo/)`);
    expect(String(page.getByText('Hello').filter({ hasText: 'wo"rld\n' }))).toBe(`getByText('Hello').filter({ hasText: 'wo"rld\\n' })`);
    expect(String(page.getByText('Hello').filter({ hasText: /wo\/\srld\n/ }))).toBe(`getByText('Hello').filter({ hasText: /wo\\/\\srld\\n/ })`);
    expect(String(page.getByText('Hello').filter({ hasText: /wor"ld/ }))).toBe(`getByText('Hello').filter({ hasText: /wor"ld/ })`);
    expect(String(page.getByText('Hello').filter({ hasNotText: 'wo"rld\n' }))).toBe(`getByText('Hello').filter({ hasNotText: 'wo"rld\\n' })`);
    expect(String(page.getByText('Hello').filter({ visible: false }).locator('div'))).toBe(`getByText('Hello').filter({ visible: false }).locator('div')`);
    expect(String(page.getByText('Hello').filter({ has: page.locator('div').getByText('bye') }))).toBe(`getByText('Hello').filter({ has: locator('div').getByText('bye') })`);
    expect(String(page.getByText('Hello').filter({ hasNot: page.locator('div').getByText('bye') }))).toBe(`getByText('Hello').filter({ hasNot: locator('div').getByText('bye') })`);
  });

  test('reverse engineers nested has and hasNot', async ({ page }) => {
    const has = page
      .locator('section')
      .filter({ has: page.locator('div').filter({ has: page.locator('span') }) })
      .filter({ hasText: 'foo' })
      .filter({ has: page.locator('a') });
    expect(String(has)).toBe(`locator('section').filter({ has: locator('div').filter({ has: locator('span') }) }).filter({ hasText: 'foo' }).filter({ has: locator('a') })`);
    const hasNot = page
      .locator('section')
      .filter({ has: page.locator('div').filter({ hasNot: page.locator('span') }) })
      .filter({ hasText: 'foo' })
      .filter({ hasNot: page.locator('a') });
    expect(String(hasNot)).toBe(`locator('section').filter({ has: locator('div').filter({ hasNot: locator('span') }) }).filter({ hasText: 'foo' }).filter({ hasNot: locator('a') })`);
    const both = page.locator('section').filter({ hasText: 'foo', has: page.locator('div') }).locator('a');
    expect(String(both)).toBe(`locator('section').filter({ hasText: 'foo' }).filter({ has: locator('div') }).locator('a')`);
  });

  test('reverse engineers frames and composed locators', async ({ page }) => {
    const framed = page
      .frameLocator('iframe')
      .getByText('foo', { exact: true })
      .frameLocator('frame').first()
      .frameLocator('iframe')
      .locator('span');
    expect(String(framed)).toBe(`locator('iframe').contentFrame().getByText('foo', { exact: true }).locator('frame').first().contentFrame().locator('iframe').contentFrame().locator('span')`);
    expect(String(page.frameLocator('div >> iframe').locator('span'))).toBe(`locator('div').locator('iframe').contentFrame().locator('span')`);
    expect(String(page.getByRole('row').locator(page.getByRole('cell').nth(2)))).toBe(`getByRole('row').locator(getByRole('cell').nth(2))`);
    expect(String(page.locator('div').and(page.getByText('foo')).or(page.locator('span').locator(page.locator('a'))).first())).toBe(`locator('div').and(getByText('foo')).or(locator('span').locator(locator('a'))).first()`);
  });

  test('prints the description describe() gave it', async ({ page }) => {
    expect(String(page.locator('#submit').describe('Submit button'))).toBe('Submit button');
    expect(page.locator('#submit').describe('Submit button').toString()).toBe('Submit button');
    expect(String(page.locator('#submit').describe('Submit button').locator('span'))).toBe(`locator('#submit').locator('span')`);
  });

  test('names the locator in a web-first failure', async ({ page }) => {
    await page.setContent('<button>Hi</button>');
    const error = await expect(page.getByRole('button', { name: 'Hi' })).toHaveText('Bye', { timeout: 300 }).catch(e => e);
    expect(error.message).toContain(`Locator:  getByRole('button', { name: 'Hi' })`);
    expect(error.message).toContain(`- Expect "toHaveText" getByRole('button', { name: 'Hi' }) with timeout 300ms`);
    expect(error.message).toContain(`- waiting for getByRole('button', { name: 'Hi' })`);
    const described = await expect(page.getByRole('button').describe('the greeting')).toHaveText('Bye', { timeout: 300 }).catch(e => e);
    expect(described.message).toContain('Locator:  the greeting');
    expect(described.message).toContain(`- Expect "toHaveText" the greeting with timeout 300ms`);
    expect(described.message).toContain(`- waiting for getByRole('button')`);
  });
});
