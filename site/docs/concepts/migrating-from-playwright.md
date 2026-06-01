# Coming from Playwright

ferridriver is a Rust-first browser automation stack with a
Playwright-shaped API. If you know Playwright, the `Browser` / `Page` /
`Locator` / `Frame` / `BrowserContext` surface is familiar — but
ferridriver is built for **Rust projects**, and the whole Playwright
toolchain (test runner, BDD, MCP server, codegen, traces) has a native
ferridriver equivalent that runs from one Rust binary with **no Node in
the run path**.

This page is for two audiences:

- **Rust developers** who want Playwright's automation and testing model
  in a Rust codebase.
- Teams replacing a specific Playwright tool — `@playwright/test`,
  `playwright-bdd`, or `@playwright/mcp`.

It is **not** a "port my TypeScript test suite in place" guide:
ferridriver does not run a JS/TS test runner. Tests are written in Rust
(or as Gherkin features). JavaScript / TypeScript shows up in exactly two
places — BDD step bodies, and the `@ferridriver/node` browser API for
scripts (see the last section).

## What replaces what

| You're using (Playwright)                       | ferridriver |
|-------------------------------------------------|-------------|
| `playwright` — browser automation library       | the `ferridriver` crate (Rust). For scripting from Node/Bun, `@ferridriver/node` exposes the same API |
| `@playwright/test` — `test()` / `expect()` runner | `ferridriver-test` — `#[ferritest]`, fixtures, hooks, and `ferridriver-expect` matchers (Rust) |
| `playwright-bdd` — Gherkin on top of Playwright | `ferridriver-bdd` — **native** Gherkin, step bodies in Rust or JS/TS, run by `ferridriver bdd` |
| `@playwright/mcp` — MCP server for AI agents    | `ferridriver mcp` — **native** MCP server, single binary, no `npx` |
| `playwright codegen`                            | `ferridriver codegen` |
| `npx playwright show-trace`                     | unchanged — ferridriver writes Playwright-format traces |
| `npx playwright install`                        | `ferridriver install` (or point ferridriver at Playwright's existing browsers) |

The browser-call layer is the same idea everywhere; only the host
language and the surrounding runner change.

## Browser automation in Rust

The core move is from a TypeScript script to an `async` Rust function.
Selectors, `getBy*` accessors, `Locator` chaining, `BrowserContext`, and
tracing all port over.

```ts
// Playwright (TypeScript)
import { chromium } from 'playwright';

const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto('https://app.example.com/login');
await page.locator('#email').fill('user@example.com');
await page.getByRole('button', { name: 'Sign in' }).click();
await page.waitForURL('/dashboard');
await browser.close();
```

```rust
// ferridriver (Rust)
use ferridriver::browser_type::chromium;
use ferridriver::options::{LaunchOptions, RoleOptions};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = chromium().launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    page.goto("https://app.example.com/login", None).await?;
    page.locator("#email").fill("user@example.com").await?;
    page.get_by_role("button", &RoleOptions { name: Some("Sign in".into()), ..Default::default() })
        .click()
        .await?;
    page.wait_for_url("/dashboard").await?;

    browser.close().await?;
    Ok(())
}
```

What changes: methods are `snake_case`, calls are `await`ed and return
`Result` (use `?`), and option bags are structs (`LaunchOptions`,
`RoleOptions`, …) defaulted with `Default::default()` or passed as `None`.
The selector strings and locator semantics are identical.

## Tests: `@playwright/test` → `ferridriver-test`

`test()` / `expect()` become `#[ferritest]` plus the Rust `expect`
matchers — auto-retrying on Playwright's polling schedule.

```ts
// Playwright test
import { test, expect } from '@playwright/test';

test('loads homepage', async ({ page }) => {
  await page.goto('https://example.com');
  await expect(page).toHaveTitle('Example Domain');
});
```

```rust
// ferridriver-test
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_homepage(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://example.com", None).await?;
    expect(&page).to_have_title("Example Domain").await?;
}
```

Fixtures, hooks, projects, retries, sharding, and reporters live in
`ferridriver-test`. See [Test runner](/test-runner/overview) and the
[`expect` reference](/test-runner/expect).

## BDD: `playwright-bdd` → `ferridriver-bdd`

Your `.feature` files are plain Gherkin — they carry over unchanged.
What changes is the step layer and the runner: `ferridriver-bdd` parses
Gherkin natively and runs it through the core test runner, so there is no
`playwright-bdd` codegen step and no Playwright test process.

Write step bodies in Rust:

```rust
#[given("I navigate to {string}")]
async fn navigate(world: &mut World, url: String) { /* ... */ }

#[when("I click {string}")]
async fn click(world: &mut World, sel: String) { /* ... */ }
```

…or keep them in JavaScript / TypeScript and let ferridriver run them:

```bash
# Before (playwright-bdd): bddgen + npx playwright test
# After:
ferridriver bdd tests/features/                          # Rust steps
ferridriver bdd --steps 'steps/**/*.{js,ts}' tests/features/   # JS/TS steps
```

JS/TS steps are bundled with rolldown, compiled to QuickJS bytecode, and
executed on the embedded `ferridriver-script` engine — **no Node or Bun
in the run path**. See [BDD overview](/bdd/overview).

## MCP: `@playwright/mcp` → `ferridriver mcp`

Swap the server command in your agent config. ferridriver ships the MCP
server in the same binary — no `npx`, and you can pick the backend and
transport.

```jsonc
// Before
{ "mcpServers": { "playwright": { "command": "npx", "args": ["@playwright/mcp@latest"] } } }

// After
{ "mcpServers": { "ferridriver": { "command": "ferridriver", "args": ["mcp"] } } }
```

```bash
ferridriver mcp                                # stdio (Claude Code, Cursor, Desktop)
ferridriver mcp --transport http --port 8080   # HTTP
ferridriver mcp --backend webkit --headless    # any backend
ferridriver mcp --auto-connect chrome          # attach to a running Chrome
```

See [MCP setup](/mcp/setup).

## Using the browser API from JavaScript / Bun

`@ferridriver/node` is the **browser API only** — `Browser`,
`BrowserContext`, `Page`, `Frame`, `Locator`, `ElementHandle`, `Route`,
and friends, driving the same Rust engine. Reach for it when you want
Playwright's automation API in a Node or Bun **script** without writing
Rust.

It is **not** a test runner: there is no `test()`, `expect()`, fixtures,
or BDD in this package. For tests, BDD, or the MCP server, use the Rust
crates and the `ferridriver` CLI described above.

```ts
// @ferridriver/node — automation, not testing
import { chromium } from '@ferridriver/node';

const browser = await chromium().launch();
const page = await browser.newPageWithUrl('https://app.example.com/login');
await page.getByLabel('Email').fill('user@example.com');
await page.getByRole('button', { name: 'Sign in' }).click();
await browser.close();
```

## API differences to know

### Locator method renames

`or` and `and` are Rust keywords, so two locator combinators are renamed
(the NAPI binding uses the same names, so the TS surface matches):

| Playwright           | ferridriver           |
|----------------------|------------------------|
| `locator.or(other)`  | `locator.orLocator(other)`  |
| `locator.and(other)` | `locator.andLocator(other)` |

### Events return a numeric listener id

Playwright's `page.on(...)` returns the `Page`; you remove a listener with
`page.off(event, handler)`. ferridriver returns a numeric id:

```ts
const id = page.on('response', (data) => console.log(`${data.status} ${data.url}`));
page.off(id);
```

## Auto-waiting

ferridriver's pre-action actionability matches Playwright: before a click
it waits for the element to be **attached, visible, enabled, position-
stable** (bounding box unchanged across animation frames), and to
**actually receive the event** at the click point (no other element
occludes it). `force: true` skips the checks, same as Playwright.

## Backends and browser choice

ferridriver runs four backends behind one API; pick per launch or per
test project.

| Playwright                               | ferridriver equivalent |
|------------------------------------------|------------------------|
| `--project=chromium`                     | `chromium()` (default `cdp-pipe`), or a `[[test.projects]]` entry |
| `--project=firefox`                      | `firefox()` (default `bidi`) |
| `--project=webkit`                       | `webkit()` (default `webkit`) |
| `chromium.launch({ channel: 'chrome' })` | `LaunchOptions { executable_path: Some("..."), .. }` |
| attach to a running browser              | `--backend cdp-raw` + `chromium().connect("ws://...", Default::default())` |

Default is `cdp-pipe` (Chromium over fd 3/4 pipes). On a protocol-level
issue, try `--backend cdp-raw` (CDP over WebSocket). The WebKit backend
uses Playwright's WebKit binary (`ferridriver install webkit`, or an
existing `npx playwright install webkit`, or `FERRIDRIVER_WEBKIT`) and
runs on Linux and macOS. Firefox uses WebDriver BiDi and must already be
installed (no bundled binary).

## Porting checklist

1. Decide what you're replacing — library, test runner, BDD, or MCP — and
   pick the ferridriver piece from the table above.
2. For automation in Rust: translate scripts to `async fn` with `?`;
   `snake_case` methods; option structs. Selectors are unchanged.
3. For automation from JS/Bun: install `@ferridriver/node` and swap the
   `playwright` import (browser code is unchanged). Remember it has no
   test runner.
4. For tests: move bodies to Rust `#[ferritest]` + `expect`, or to Gherkin
   features run by `ferridriver bdd`.
5. Rename `.or()` → `.orLocator()` and `.and()` → `.andLocator()`.
6. Add a `backend` to project configs only if you want something other
   than Chromium-over-pipes.
