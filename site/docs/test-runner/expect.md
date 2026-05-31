# expect

Auto-retrying assertions. All polling, actionability checks, and retries
run inside the Rust core (`ferridriver-expect`) — the JavaScript /
TypeScript binding is a thin wrapper, so the retry loop never crosses
the language boundary.

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

## JavaScript / TypeScript matchers

The `expect` global is available in `run_script`, in BDD JS / TS step
bodies, and in extensions. It is a thin QuickJS binding
(`ferridriver-script`) over the same `ferridriver-expect` core, so every
matcher delegates to the Rust implementation. String matchers also
accept a native `RegExp`.

`expect(value | locator | page | apiResponse | fn)` dispatches on the
runtime type:

**Value (Jest-style):** `toBe`, `toEqual`, `toStrictEqual`, `toBeNull`,
`toBeUndefined`, `toBeDefined`, `toBeTruthy`, `toBeFalsy`, `toBeNaN`,
`toBeCloseTo`, `toBeGreaterThan`, `toBeGreaterThanOrEqual`,
`toBeLessThan`, `toBeLessThanOrEqual`, `toContain`, `toContainEqual`,
`toHaveLength`, `toHaveProperty`, `toMatch`, `toMatchObject`,
`toBeInstanceOf`, `toThrow`.

**Page:** `toHaveTitle`, `toHaveURL`.

**Locator — visibility / state:** `toBeVisible`, `toBeHidden`,
`toBeEnabled`, `toBeDisabled`, `toBeChecked`, `toBeEditable`,
`toBeAttached`, `toBeEmpty`.

**Locator — text / value / attributes:** `toHaveText`, `toContainText`,
`toHaveValue`, `toHaveCount`, `toHaveAttribute`.

**APIResponse:** `toBeOK`.

**Poll:** `expect.poll(fn, { timeout? }).toBe` / `.toEqual` /
`.toSatisfy`.

**Asymmetric:** `expect.any`, `expect.anything`,
`expect.arrayContaining`, `expect.objectContaining`,
`expect.stringContaining`, `expect.stringMatching`, `expect.closeTo`,
plus the `expect.not.*` shorthand.

Modifiers: `.not` (a getter returning a negated proxy), `.soft()` (or
`expect.soft(...)`), `.withTimeout(ms)`, and `.withMessage(msg)`.

## Retry cadence

Polling schedule follows Playwright: `100, 250, 500, 1000` ms, then
`1000` ms thereafter. The total wait is capped by `expectTimeout`
(default 5000 ms). Polling and actionability checks are implemented in
Rust — the JS binding issues a single async call per assertion and the
core loop decides when to re-check.
