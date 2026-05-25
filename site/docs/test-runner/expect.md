# expect

Auto-retrying assertions. All polling, actionability checks, and retries
run inside the Rust core (`ferridriver-expect`) — the TypeScript bindings
are thin wrappers so there are no NAPI round-trips per retry.

## Rust matchers (38)

Modifiers on every matcher: `.not()`, `.with_timeout()`, `.soft()`,
`.with_message()`. Page / URL / value matchers accept
`impl Into<StringOrRegex>`, so you can pass either a `&str` or a regex.

### Page (4)

| Matcher              | Description |
|----------------------|-------------|
| `to_have_title`      | Page title matches string or regex |
| `to_contain_title`   | Page title contains substring |
| `to_have_url`        | Page URL matches string or regex |
| `to_contain_url`     | Page URL contains substring |

### Locator — visibility / state (10)

`to_be_visible`, `to_be_hidden`, `to_be_enabled`, `to_be_disabled`,
`to_be_checked`, `to_be_editable`, `to_be_attached`, `to_be_empty`,
`to_be_focused`, `to_be_in_viewport`

### Locator — text / value (6)

`to_have_text`, `to_contain_text`, `to_have_value`, `to_have_values`,
`to_have_texts`, `to_contain_texts`

### Locator — attributes (9)

`to_have_attribute`, `to_have_class`, `to_contain_class`, `to_have_css`,
`to_have_id`, `to_have_role`, `to_have_accessible_name`,
`to_have_accessible_description`, `to_have_accessible_error_message`

### Locator — other (5)

`to_have_js_property`, `to_have_count`, `to_match_snapshot`,
`to_have_screenshot`, `to_match_aria_snapshot`

### Poll / satisfy (4)

- `to_equal` — polled value equals expected
- `to_satisfy` — polled value passes a user predicate
- `to_pass` — run an async closure until it succeeds
- `to_pass_with_options` — `to_pass` with custom `intervals` / `timeout`

## TypeScript matchers

The NAPI wrapper currently exposes a subset. All take `string`
arguments — regex is a Rust-only affordance today. Missing matchers
fall back to composing `toPass` with a `page.evaluate` or a locator
method.

**Page (2):** `toHaveTitle`, `toHaveURL`.

**Locator — visibility / state (5):** `toBeVisible`, `toBeHidden`,
`toBeEnabled`, `toBeDisabled`, `toBeChecked`.

**Locator — other (5):** `toHaveText`, `toContainText`, `toHaveValue`,
`toHaveAttribute`, `toHaveCount`.

**Poll (1):** `toPass(options)`.

All TS matchers support `.not` (a getter returning a negated proxy) and
a `timeout` option passed through the fluent API.

## Retry cadence

Polling schedule follows Playwright: `100, 250, 500, 1000` ms, then
`1000` ms thereafter. The total wait is capped by `expectTimeout`
(default 5000 ms). Polling and actionability checks are implemented in
Rust — the TS wrapper issues a single async NAPI call per assertion and
the core loop decides when to re-check.
