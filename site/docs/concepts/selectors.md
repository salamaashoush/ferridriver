# Selectors

Every call to `page.locator(...)` or `locator.locator(...)` takes a **selector string**. ferridriver implements the full Playwright selector engine in Rust: each selector compiles to a pipeline of parts that run against the DOM via a small IIFE injected once per page.

You usually don't need the engine prefixes — CSS is the default, and helpers like `get_by_role` build a correct rich selector for you.

## CSS is the default

Any string without a recognized engine prefix is treated as CSS.

```rust
page.locator("#email")
page.locator(".btn.primary")
page.locator("button[type=submit]")
page.locator("form > input:first-child")
```

## Engines

Prefix a part with `engine=` to switch engines. Engines are:

| Prefix | What it matches |
|---|---|
| `css=`          | CSS selectors (the default; prefix is optional) |
| `text=`          | Visible text, substring by default, quoted string for exact (`text="Hello"`) |
| `role=`          | ARIA role, with optional `[name=...]`, `[level=...]`, `[checked=true]`, etc. |
| `label=`         | Form label text (picks the associated control) |
| `placeholder=`   | Input `placeholder` attribute |
| `alt=`           | Image `alt` attribute |
| `title=`         | Any element's `title` attribute |
| `testid=`        | `data-testid` attribute |
| `xpath=`         | XPath expression |
| `id=`            | Exact `id` attribute |
| `nth=N`          | Pick the Nth result from the previous part (0-indexed) |
| `visible=true`   | Filter previous part to visible elements only |
| `has=...`        | Keep only elements that have a descendant matching the inner selector |
| `has-text=...`   | Keep only elements whose text contains the argument |
| `has-not=...`    | Inverse of `has` |
| `has-not-text=...` | Inverse of `has-text` |

## Chaining with `>>`

The `>>` operator narrows scope: each part's results become the roots for the next part's query.

```rust
// Find buttons named "Delete" inside the current row
page.locator("css=tr.row >> role=button[name=Delete]")

// First item in a list that contains "Keep"
page.locator("css=li >> has-text=Keep >> nth=0")

// All h2 elements that are visible and contain "Section"
page.locator("css=h2 >> visible=true >> has-text=Section")
```

## Role selectors

`role=` understands every ARIA role plus these refiners (Playwright-compatible):

```
role=button                         any button
role=button[name="Save"]            button with accessible name "Save"
role=button[name=/^Save/i]          regex match on accessible name
role=heading[level=2]
role=checkbox[checked=true]
role=tab[selected=true]
role=link[expanded=false]
role=option[pressed=false]
role=menuitem[disabled=true]
role=textbox[exact=true]            exact name match (default is substring)
role=img[include-hidden=true]       include ARIA-hidden elements
```

## Text and label options

Text-based engines accept options from their typed getter equivalents:

```rust
use ferridriver::options::TextOptions;

// Rust: exact string, case-sensitive
let opts = TextOptions { exact: Some(true), ..Default::default() };
page.get_by_text("Sign in", &opts);

// Rust: regex
let opts = TextOptions { regex: Some("^Sign".to_string()), ..Default::default() };
page.get_by_text("", &opts);
```

In raw selector strings:

```
text="Hello"            exact match (quoted)
text=Hello              substring (unquoted)
text=/^Hello$/i         regex with flags
```

## Typed getters

The `get_by_*` helpers build the right selector for you and are the preferred API when you have something accessible to target.

**Rust (`Page`, `Frame`, `Locator`):**

```rust
page.get_by_role("button", &RoleOptions { name: Some("Save".into()), ..Default::default() });
page.get_by_text("Hello", &TextOptions::default());
page.get_by_label("Email", &TextOptions::default());
page.get_by_placeholder("you@example.com", &TextOptions::default());
page.get_by_alt_text("Logo", &TextOptions::default());
page.get_by_title("Settings", &TextOptions::default());
page.get_by_test_id("login-form");
```

**TypeScript (NAPI):**

```ts
page.getByRole('button', { name: 'Save' });
page.getByText('Hello');
page.getByLabel('Email');
page.getByPlaceholder('you@example.com');
page.getByAltText('Logo');
page.getByTitle('Settings');
page.getByTestId('login-form');
```

## Custom test ID attribute

By default, `testid=` and `getByTestId` look at `data-testid`. Override per context:

```rust
browser.new_context_with_options(ContextOptions {
  test_id_attribute: Some("data-qa".to_string()),
  ..Default::default()
}).await?;
```

## Performance notes

- The selector-engine JavaScript is injected **once per page** via `addInitScript` and re-bootstrapped after navigation. Repeated selectors don't reparse the engine.
- Each selector is parsed to a `Selector { parts: Vec<SelectorPart> }` **in Rust** (see `crates/ferridriver/src/selectors.rs`) and then compiled into a single IIFE — no per-query JS eval.
- `locator(...)` is lazy: no DOM query happens until you call an action (`click`, `fill`, ...) or an assertion (`to_be_visible`, `to_have_text`, ...).
