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

`expect(value | locator | page | apiResponse | fn, messageOrOptions?)`
keeps the value it was handed and dispatches the web-first matchers on
its runtime type. The value matchers apply to ANY subject — a function,
a `Locator` and a `Page` all answer `toBe`:

**Value (Jest-style):** `toBe`, `toEqual`, `toStrictEqual`, `toBeNull`,
`toBeUndefined`, `toBeDefined`, `toBeTruthy`, `toBeFalsy`, `toBeNaN`,
`toBeCloseTo`, `toBeGreaterThan`, `toBeGreaterThanOrEqual`,
`toBeLessThan`, `toBeLessThanOrEqual`, `toContain`, `toContainEqual`,
`toHaveLength`, `toHaveProperty`, `toMatch`, `toMatchObject`,
`toBeInstanceOf`, `toThrow`.

The identity- and type-sensitive ones read the live value, so they mean
what they mean in Playwright: `toBe` is `Object.is` (two structurally
equal objects are NOT `toBe`-equal — use `toEqual`), `toBeInstanceOf` is
the `instanceof` operator, `toContain` is a substring test on a string
and `[...received].indexOf` on any iterable, `toHaveLength` reads the
receiver's own `.length` (a function's arity included), and `null` and
`undefined` are distinct. Calling one on a receiver it cannot work on —
`toContain` on `null`, `toHaveLength` on a value without a numeric
`.length` — throws a `TypeError` that `.not` does not flip, as upstream.

The structural matchers run jest's own equality over the live values:
`toEqual` ignores `undefined`-valued keys on both sides, `toStrictEqual`
adds the constructor check and array sparseness, `toMatchObject` is a
recursive subset, and `Map` / `Set` / `Date` / `RegExp` / `Error` /
typed arrays / `bigint` each compare as themselves rather than as
whatever they serialize to. Cyclic structures terminate.
`toHaveProperty` READS the property, so a getter or an inherited field
answers.

The same engine is available to Rust: `expect_value(json)` runs it over
`serde_json::Value`, which implements the same `LiveValue` trait and
degrades exactly where JSON has nothing to say (no `undefined`, no
`Map`, no identity).

**Page:** `toHaveTitle`, `toHaveURL`.

**Locator — visibility / state:** `toBeVisible`, `toBeHidden`,
`toBeEnabled`, `toBeDisabled`, `toBeChecked`, `toBeEditable`,
`toBeAttached`, `toBeEmpty`.

**Locator — text / value / attributes:** `toHaveText`, `toContainText`,
`toHaveValue`, `toHaveCount`, `toHaveAttribute`.

**APIResponse:** `toBeOK`.

**Poll:** `expect.poll(fn, messageOrOptions?).toBe` / `.toEqual` /
`.toSatisfy`, where the options are `{ message?, timeout?, intervals? }`.
`.toBe` polls for `Object.is` against the generated value, `.toEqual`
for structural equality.

**Asymmetric:** `expect.any`, `expect.anything`,
`expect.arrayContaining`, `expect.arrayOf`, `expect.objectContaining`,
`expect.stringContaining`, `expect.stringMatching`, `expect.closeTo`,
plus the `expect.not.*` shorthand. Every matcher registered through
`expect.extend` is published as an asymmetric one too, so
`expect.toBeX()` can stand in for a value inside `toEqual` /
`toMatchObject`; an async matcher cannot (the comparison is synchronous,
as it is upstream).

**Settled:** `.resolves` and `.rejects` settle the subject — a promise,
or a function returning one — and then run the ordinary matcher against
what it settled to, so every matcher under them returns a Promise and
must be awaited. `.resolves.not` / `.rejects.not` compose one level deep,
as upstream. The settled value is dispatched afresh, so a promise
resolving to a `Locator` reaches the Locator matchers; under `.rejects`
the rejection reason IS the subject, and `toThrow` reads it as the thrown
error rather than calling it. `expect.poll(...).resolves` is refused with
Playwright's message.

**Custom:** `expect.extend({ toBeX(received, ...args) { … } })` returns a
new expect carrying the matcher; a name that is not a built-in also
becomes available on the expect `extend` was called on, so the common
`expect.extend({...})` with the result discarded works. A built-in name
is only shadowed on the returned expect. The body reads `this.isNot`,
`this.isSoft`, `this.promise`, `this.timeout` and a `this.utils` subset,
returns `{ pass, message?, expected?, actual?, log? }`, and may be
async. `expect.configure({ message?, timeout?, soft? })` returns a
configured expect, `expect.soft` is a getter answering the soft one,
`expect.getState()` answers an object, and `mergeExpects(a, b)` builds
one expect exposing every matcher of both. Custom matchers reach `.`,
`.not`, `.resolves` and `.rejects` alike.

The same matchers work from Rust: `ferridriver_expect::matcher(f)` plus
`expect_value(v).matches("toBeX", &m, &args)` runs a plain Rust function
through the identical context, verdict and message path.

**Soft:** `expect.soft(...)` (and `expect.configure({ soft: true })`)
records a failure against the running test and carries on; the test
fails at the end with every soft failure listed, and `testInfo.errors`
holds them meanwhile. Value, web-first and custom matchers all obey it.
Outside a test there is nothing to record into, so a soft failure is
raised normally rather than vanishing — the same rule on both sides:
Rust tests get it from `ferridriver_expect::soft`, which the runner
scopes around each test body.

Modifiers: `.not` (a getter returning a negated proxy), `.soft()` (or
`expect.soft(...)`), `.withTimeout(ms)`, and `.withMessage(msg)`.

## Retry cadence

Polling schedule follows Playwright: `100, 250, 500, 1000` ms, then
`1000` ms thereafter. The total wait is capped by `expectTimeout`
(default 5000 ms). Polling and actionability checks are implemented in
Rust — the JS binding issues a single async call per assertion and the
core loop decides when to re-check.
