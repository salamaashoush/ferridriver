//! §3.12 Rule-9 integration tests: `getBy*` matchers accept a JS
//! `RegExp` in addition to literal strings across every backend.
//!
//! Exercises the QuickJS `run_script` binding end-to-end: real
//! `RegExp` instance → `string_or_regex_from_js` → core
//! `StringOrRegex::Regex { source, flags }` →
//! `build_*_selector` escape → Playwright's injected engine regex
//! matcher. The final `count()` / `textContent()` call is the DOM
//! truth — if any step silently drops the regex semantics the count
//! is wrong.
//!
//! Every backend runs the same assertions (cdp-pipe / cdp-raw /
//! bidi / webkit) — no silent skips. WebKit's injected engine is
//! the same verbatim-Playwright code as CDP/BiDi, so regex matching
//! works identically.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;

/// `page.getByText(/hello \d+/)` — regex flow matches only the numeric
/// entries; literal `"hello"` substring would over-match.
pub fn test_getby_text_regex(c: &mut McpClient) {
  c.nav_url("data:text/html,<p>hello world</p><p>hello 42</p><p>hello 7</p><p>HELLO 9</p>");
  let script = r#"
    // Regex: matches "hello " followed by digits, case-sensitive.
    const re = /hello \d+/;
    const loc = page.getByText(re);
    const count = await loc.count();
    // Case-insensitive flag includes HELLO 9 → total 3 matches.
    const reI = /hello \d+/i;
    const countI = await page.getByText(reI).count();
    return { count, countI };
  "#;
  let v = c.script_value(script);
  assert_eq!(
    v["count"].as_i64(),
    Some(2),
    "getByText(/hello \\d+/) should match only 'hello 42' and 'hello 7': {v}",
  );
  assert_eq!(
    v["countI"].as_i64(),
    Some(3),
    "getByText(/hello \\d+/i) should match 'hello 42', 'hello 7', and 'HELLO 9': {v}",
  );
}

/// `page.getByRole('button', { name: /submit/i })` — regex name filter.
pub fn test_getby_role_name_regex(c: &mut McpClient) {
  c.nav_url("data:text/html,<button>Submit form</button><button>submit data</button><button>Cancel</button>");
  let script = r"
    const count = await page.getByRole('button', { name: /submit/i }).count();
    // Literal 'Submit' (no regex) matches case-insensitively by default but
    // with substring semantics on the accessible name — should also be 2.
    const countLiteral = await page.getByRole('button', { name: 'submit' }).count();
    return { count, countLiteral };
  ";
  let v = c.script_value(script);
  assert_eq!(
    v["count"].as_i64(),
    Some(2),
    "getByRole('button', {{name: /submit/i}}) should match 'Submit form' and 'submit data': {v}",
  );
  assert_eq!(
    v["countLiteral"].as_i64(),
    Some(2),
    "getByRole('button', {{name: 'submit'}}) should also match case-insensitively: {v}",
  );
}

/// `page.getByPlaceholder(/email/i)` against attribute-typed matcher.
pub fn test_getby_placeholder_regex(c: &mut McpClient) {
  c.nav_url(
    "data:text/html,<input placeholder='Enter Email'><input placeholder='Your email'><input placeholder='Phone'>",
  );
  let script = r"
    const count = await page.getByPlaceholder(/email/i).count();
    return { count };
  ";
  let v = c.script_value(script);
  assert_eq!(
    v["count"].as_i64(),
    Some(2),
    "getByPlaceholder(/email/i) should match both email inputs: {v}",
  );
}

/// `page.getByTestId(/card-\d+/)` with regex test-id.
pub fn test_getby_test_id_regex(c: &mut McpClient) {
  c.nav_url(
    "data:text/html,<div data-testid='card-1'>A</div><div data-testid='card-42'>B</div><div data-testid='other'>C</div>",
  );
  let script = r"
    const count = await page.getByTestId(/card-\d+/).count();
    return { count };
  ";
  let v = c.script_value(script);
  assert_eq!(
    v["count"].as_i64(),
    Some(2),
    "getByTestId(/card-\\d+/) should match 'card-1' and 'card-42': {v}",
  );
}
