# ferridriver-expect

[![crates.io](https://img.shields.io/crates/v/ferridriver-expect.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-expect)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-expect?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-expect)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Auto-retrying assertion library for ferridriver. Polls on the Playwright
schedule (`100, 250, 500, 1000, 1000, ...` ms) up to `expectTimeout`
(default 5000 ms). All polling, actionability checks, and diff rendering
run in the Rust core — bindings are thin shims.

This crate is consumed via `ferridriver_test::prelude::expect` /
`expect_value` / `expect_poll`. Depend on `ferridriver-expect` directly
only when embedding outside the standard test runner.

## Surface

Matchers live on three builder roots:

- **`expect(locator)`** — `Expect<Locator>` (visibility, state, text,
  value, attributes, class, ARIA, layout, count, snapshots).
- **`expect(page)`** — `Expect<Page>` (title, URL).
- **`expect(response)`** — `Expect<ApiResponse>` (status, headers, body
  predicates).

Plus polling utilities:

- **`expect_poll(closure, timeout)`** — poll a value-producing closure
  until it equals expected.
- **`to_pass(body)` / `to_pass_with_options(body, options)`** — retry an
  async block until it returns `Ok(())`.
- **`expect_value(value)`** — `Expect<Value>` matchers (`to_equal`,
  `to_contain`, `to_match`, asymmetric matchers for partial-shape match).

## Modifiers

Every matcher supports:

| Modifier            | Effect |
|---------------------|--------|
| `.not()`            | Invert the assertion (still polls). |
| `.with_timeout(d)`  | Override default `expectTimeout` for this call. |
| `.with_message(s)`  | Attach a custom message to the failure. |
| `.soft()`           | Record the failure but do not throw — `TestInfo` aggregates and fails the test at the end. |

## Matcher list (38)

**Page (4):** `to_have_title`, `to_contain_title`, `to_have_url`, `to_contain_url`.

**Locator visibility / state (10):** `to_be_visible`, `to_be_hidden`,
`to_be_enabled`, `to_be_disabled`, `to_be_checked`, `to_be_editable`,
`to_be_attached`, `to_be_empty`, `to_be_focused`, `to_be_in_viewport`.

**Locator text / value (6):** `to_have_text`, `to_contain_text`,
`to_have_value`, `to_have_values`, `to_have_texts`, `to_contain_texts`.

**Locator attributes (9):** `to_have_attribute`, `to_have_class`,
`to_contain_class`, `to_have_css`, `to_have_id`, `to_have_role`,
`to_have_accessible_name`, `to_have_accessible_description`,
`to_have_accessible_error_message`.

**Locator other (5):** `to_have_js_property`, `to_have_count`,
`to_match_snapshot`, `to_have_screenshot`, `to_match_aria_snapshot`.

**Poll / satisfy (4):** `to_equal`, `to_satisfy`, `to_pass`,
`to_pass_with_options`.

## License

MIT OR Apache-2.0
