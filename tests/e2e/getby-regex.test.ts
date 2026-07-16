// Ported from crates/ferridriver-cli/tests/backends_support/getby_regex.rs —
// `getBy*` matchers accept a JS RegExp in addition to literal strings.
// A real RegExp instance flows string_or_regex -> core StringOrRegex ->
// selector build -> Playwright's injected engine regex matcher; the
// final count() is the DOM truth — if any step drops the regex
// semantics the count is wrong. Identical assertions on every backend.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('getby regex', () => {
  test('getby_text_regex', async ({ page }) => {
    await page.goto(dataUrl('<p>hello world</p><p>hello 42</p><p>hello 7</p><p>HELLO 9</p>'));
    // Case-sensitive: matches only the numeric entries; literal "hello"
    // substring would over-match.
    await expect(page.getByText(/hello \d+/)).toHaveCount(2);
    // Case-insensitive flag includes HELLO 9.
    await expect(page.getByText(/hello \d+/i)).toHaveCount(3);
  });

  test('getby_role_name_regex', async ({ page }) => {
    await page.goto(dataUrl('<button>Submit form</button><button>submit data</button><button>Cancel</button>'));
    await expect(page.getByRole('button', { name: /submit/i })).toHaveCount(2);
    // Literal name matches case-insensitively with substring semantics
    // on the accessible name — also 2.
    await expect(page.getByRole('button', { name: 'submit' })).toHaveCount(2);
  });

  test('getby_placeholder_regex', async ({ page }) => {
    await page.goto(dataUrl("<input placeholder='Enter Email'><input placeholder='Your email'><input placeholder='Phone'>"));
    await expect(page.getByPlaceholder(/email/i)).toHaveCount(2);
  });

  test('getby_test_id_regex', async ({ page }) => {
    await page.goto(
      dataUrl("<div data-testid='card-1'>A</div><div data-testid='card-42'>B</div><div data-testid='other'>C</div>"),
    );
    await expect(page.getByTestId(/card-\d+/)).toHaveCount(2);
  });
});
