// `use: { testIdAttribute }` — which attribute `getByTestId` reads.
//
// Every case is an INVERSION, not a count: the page carries both
// `data-testid` and `data-test-id`, and each holds the id the other
// element would answer to. A selector that ignored the configured
// attribute would still find an element, so only swapping which element
// comes back proves the attribute reached the selector.
//
// Runs on all four backends because the matching happens in the page's
// injected engine, and the comma form goes through a different engine
// than the single-attribute one.

import { test, describe, expect } from '@ferridriver/test';

const BOTH_ATTRIBUTES = `
  <div data-testid="alpha" id="by-testid">from data-testid</div>
  <div data-test-id="alpha" id="by-test-id">from data-test-id</div>
  <div data-pw="beta" id="by-pw">from data-pw</div>
`;

describe('testIdAttribute', () => {
  test('the default is data-testid', async ({ page }) => {
    await page.setContent(BOTH_ATTRIBUTES);
    await expect(page.getByTestId('alpha')).toHaveAttribute('id', 'by-testid');
  });

  describe('with an overridden attribute', () => {
    test.use({ testIdAttribute: 'data-test-id' });

    test('getByTestId reads it instead of data-testid', async ({ page }) => {
      await page.setContent(BOTH_ATTRIBUTES);
      // The same id now resolves to the OTHER element.
      await expect(page.getByTestId('alpha')).toHaveAttribute('id', 'by-test-id');
    });

    test('a locator chain reads it too', async ({ page }) => {
      await page.setContent(`<section id="wrap">${BOTH_ATTRIBUTES}</section>`);
      await expect(page.locator('#wrap').getByTestId('alpha')).toHaveAttribute('id', 'by-test-id');
    });

    test('frameLocator().getByTestId reads it inside an iframe', async ({ page }) => {
      // Proves the attribute reached the iframe's own injected script,
      // which is a separate document from the main frame's.
      await page.setContent(
        `<iframe id="f" srcdoc="${BOTH_ATTRIBUTES.replace(/"/g, '&quot;')}"></iframe>`,
      );
      await expect(page.frameLocator('#f').getByTestId('alpha')).toHaveAttribute('id', 'by-test-id');
    });
  });

  describe('with the comma form', () => {
    // Playwright's multi-attribute form: any of the named attributes
    // matches, which is a different engine path than a single name.
    test.use({ testIdAttribute: 'data-pw,data-ti' });

    test('either attribute matches', async ({ page }) => {
      await page.setContent(`
        <div data-pw="one" id="by-pw">pw</div>
        <div data-ti="two" id="by-ti">ti</div>
        <div data-testid="three" id="by-default">default</div>
      `);
      await expect(page.getByTestId('one')).toHaveAttribute('id', 'by-pw');
      await expect(page.getByTestId('two')).toHaveAttribute('id', 'by-ti');
      // The default attribute is no longer consulted.
      await expect(page.getByTestId('three')).toHaveCount(0);
    });
  });

  test('the override does not leak into a test that did not ask for it', async ({ page }) => {
    await page.setContent(BOTH_ATTRIBUTES);
    await expect(page.getByTestId('alpha')).toHaveAttribute('id', 'by-testid');
  });
});
