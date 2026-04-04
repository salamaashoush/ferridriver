# ferridriver

High-performance browser automation library in Rust with a Playwright-compatible API. Multiple CDP backends, native WebKit, MCP server for AI agents, Node.js/Bun bindings, and a full test runner with component testing for 6 frameworks.

## Architecture

```
ferridriver (core library)
  ├── CdpPipe backend       Chrome via fd 3/4 pipes — fastest, default
  ├── CdpRaw backend        Chrome via WebSocket — connect to running browser
  ├── WebKit backend         macOS WKWebView — native accessibility
  │
  ├── ferridriver-cli        CLI: MCP server + test runner (Rust)
  ├── ferridriver-napi       Node.js/Bun bindings (NAPI-RS)
  ├── @ferridriver/test      CLI: test runner + component testing (TypeScript)
  │
  ├── ferridriver-test       Test runner core: parallel, hooks, expect, reporters
  ├── ferridriver-ct-leptos  Component testing for Leptos (trunk)
  ├── ferridriver-ct-dioxus  Component testing for Dioxus (dx)
  │
  ├── @ferridriver/ct-core   JS CT core: Vite plugin, import transform, browser runtime
  ├── @ferridriver/ct-react  React adapter (createRoot/render)
  ├── @ferridriver/ct-vue    Vue adapter (createApp/mount)
  ├── @ferridriver/ct-svelte Svelte adapter (mount, Svelte 4+5)
  └── @ferridriver/ct-solid  Solid adapter (render/dispose)
```

## Quick Start (Rust)

```rust
use ferridriver::{Browser, Page};
use ferridriver::options::LaunchOptions;

#[tokio::main]
async fn main() -> Result<(), String> {
    let browser = Browser::launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    page.goto("https://example.com", None).await?;
    page.locator("#email").fill("test@example.com").await?;
    page.locator("button[type=submit]").click().await?;
    page.wait_for_url("/dashboard").await?;

    let title = page.title().await?;
    let png = page.screenshot(Default::default()).await?;

    browser.close().await?;
    Ok(())
}
```

## Quick Start (Node.js/Bun)

```typescript
import { Browser } from 'ferridriver';

const browser = await Browser.launch();
const page = await browser.newPageWithUrl('https://example.com');
await page.locator('h1').click();
console.log(await page.locator('h1').textContent());
await browser.close();
```

## Test Runner

Parallel test execution with auto-retrying assertions. 99 tests/sec, 4x faster than Playwright Test.

### Rust

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn login_flow(page: Page) -> Result<(), TestFailure> {
    page.goto("https://app.example.com/login", None).await?;
    page.locator("#email").fill("user@example.com").await?;
    page.locator("button[type=submit]").click().await?;
    expect(&page).to_have_url("dashboard").await?;
    Ok(())
}
```

### TypeScript

```typescript
import { test, expect } from '@ferridriver/test';

test('login flow', async ({ page }) => {
  await page.goto('https://app.example.com/login');
  await page.locator('#email').fill('user@example.com');
  await page.locator('button[type=submit]').click();
  await expect(page).toHaveURL(/dashboard/);
});
```

```bash
ferridriver-test tests/login.spec.ts --workers 4
```

### Features

- **Parallel**: N workers × N browsers, MPMC work-stealing dispatch
- **Hooks**: beforeAll/afterAll, beforeEach/afterEach
- **Serial mode**: tests run in order, skip remaining on failure
- **Expected failures**: `test.fail()` pass/fail inversion
- **Global setup/teardown**
- **Retry + flaky detection**
- **Reporters**: Terminal, JUnit XML, JSON, HTML
- **Text snapshots**: `.snap` files with unified diff
- **Visual snapshots**: pixel-level PNG diff with threshold and diff image
- **CDP tracing**: Playwright-compatible format

### 32 Expect Matchers

Visibility: `toBeVisible`, `toBeHidden`, `toBeAttached`, `toBeInViewport`
State: `toBeEnabled`, `toBeDisabled`, `toBeChecked`, `toBeEditable`, `toBeFocused`, `toBeEmpty`
Text: `toHaveText`, `toContainText`, `toHaveTexts`, `toContainTexts`
Value: `toHaveValue`, `toHaveValues`
Attributes: `toHaveAttribute`, `toHaveClass`, `toContainClass`, `toHaveCSS`, `toHaveId`, `toHaveRole`
A11y: `toHaveAccessibleName`, `toHaveAccessibleDescription`, `toMatchAriaSnapshot`
Snapshots: `toMatchSnapshot`, `toHaveScreenshot`
Other: `toHaveJSProperty`, `toHaveCount`
Page: `toHaveTitle`, `toHaveURL`
Modifiers: `.not()`, `.withTimeout()`, `.soft()`, `.withMessage()`
Utilities: `expect.poll()`, `toPass()`

## Component Testing

Test UI components in real browsers. Supports Rust WASM and JS frameworks with framework-native toolchains.

### Leptos

```rust
use ferridriver_ct_leptos::prelude::*;

#[component_test]
async fn counter_increments(page: Page) -> Result<(), TestFailure> {
    page.locator("#inc").click().await?;
    expect(&page.locator("#count")).to_have_text("1").await?;
    Ok(())
}

ferridriver_ct_leptos::main!();
```

```bash
cargo install trunk
cargo test -p my-leptos-app --test components
```

### Dioxus

```rust
use ferridriver_ct_dioxus::prelude::*;

#[component_test]
async fn counter_increments(page: Page) -> Result<(), TestFailure> {
    page.locator("#inc").click().await?;
    expect(&page.locator("#count")).to_have_text("1").await?;
    Ok(())
}

ferridriver_ct_dioxus::main!();
```

```bash
cargo install dioxus-cli
cargo test -p my-dioxus-app --test components
```

### React / Vue / Svelte / Solid

```typescript
import { test, expect } from '@ferridriver/test';

test('counter increments', async ({ page }) => {
  await page.locator('#inc').click();
  await expect(page.locator('#count')).toHaveText('1');
});
```

```bash
ferridriver-test --ct --framework react src/todomvc.ct.ts
ferridriver-test --ct --framework vue src/todomvc.ct.ts
ferridriver-test --ct --framework svelte src/todomvc.ct.ts
ferridriver-test --ct --framework solid src/todomvc.ct.ts
```

The `--ct` flag starts the Vite dev server, pre-warms it, navigates each test page to the app, and provides a `mount()` fixture.

### How It Works

**Rust frameworks**: `trunk build` / `dx build` (cached) → `ComponentServer` serves static output → ferridriver-test parallel runner creates pages against it. Custom harness with `inventory` for test discovery.

**JS frameworks**: CLI starts Vite dev server → pre-warms compilation → NAPI test runner creates pages navigated to `baseUrl` → tests interact via Playwright-style Page/Locator API.

### Performance

| Framework | 15 TodoMVC tests | Per test |
|-----------|-----------------|---------|
| Solid | 392ms | 26ms |
| Vue | 409ms | 27ms |
| Svelte | 447ms | 30ms |
| Leptos | 483ms | 32ms |
| React | 534ms | 36ms |
| Dioxus | 599ms | 40ms |

500 Leptos tests: 10.1s (49.5 tests/sec)

## MCP Server

25 tools for AI agent browser automation. Works with Claude, Cursor, or any MCP client.

```bash
# stdio (for Claude Code)
ferridriver mcp

# HTTP (for remote clients)
ferridriver mcp --transport http --port 8080
```

Tools: `navigate`, `page`, `click`, `click_at`, `hover`, `fill`, `fill_form`, `type_text`, `press_key`, `drag`, `scroll`, `select_option`, `upload_file`, `snapshot`, `screenshot`, `evaluate`, `wait_for`, `search_page`, `get_markdown`, `cookies`, `storage`, `emulate`, `diagnostics`, `list_steps`, `run_scenario`

## BDD Framework

58 Gherkin step definitions for browser automation testing.

```gherkin
Feature: Login
  Scenario: Successful login
    Given I navigate to "https://app.example.com/login"
    When I fill "#email" with "user@example.com"
    And I click "#submit"
    Then the URL should contain "/dashboard"
```

## Page API

### Navigation
`goto`, `goBack`, `goForward`, `reload`, `url`, `title`, `content`, `waitForUrl`, `waitForLoadState`, `waitForNavigation`

### Locators
`locator(css)`, `getByRole`, `getByText`, `getByLabel`, `getByPlaceholder`, `getByAltText`, `getByTitle`, `getByTestId`

### Actions
`click`, `dblclick`, `rightClick`, `tap`, `fill`, `clear`, `typeText`, `press`, `pressSequentially`, `hover`, `focus`, `blur`, `scrollIntoView`, `check`, `uncheck`, `setChecked`, `selectOption`, `setInputFiles`, `selectText`, `dragTo`, `dispatchEvent`

### Queries
`textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, `boundingBox`, `isVisible`, `isHidden`, `isEnabled`, `isDisabled`, `isChecked`, `isEditable`, `isAttached`, `count`, `all`, `first`, `last`, `nth`, `allTextContents`, `allInnerTexts`, `evaluate`, `evaluateAll`, `or`, `and`, `filter`

### Screenshots & Content
`screenshot`, `screenshotElement`, `pdf`, `markdown`, `setContent`, `addScriptTag`, `addStyleTag`, `snapshotForAi`

### Network
`route(pattern, handler)`, `unroute` — fulfill, continue, or abort requests

### Events
`on`, `once`, `off`, `removeAllListeners`, `waitForEvent`, `waitForResponse`, `waitForRequest`, `waitForDownload`, `expectNavigation`, `expectResponse`, `expectRequest`

### Emulation
`setViewportSize`, `setUserAgent`, `setLocale`, `setTimezone`, `setGeolocation`, `setNetworkState`, `emulateMedia`, `setJavascriptEnabled`, `setExtraHttpHeaders`, `grantPermissions`

### Cookies & Storage
`cookies`, `setCookie`, `deleteCookie`, `clearCookies`, `storageState`, `setStorageState`

### Input Devices
`keyboard.press`, `keyboard.type`, `mouse.click`, `mouse.move`, `mouse.wheel`, `mouse.down`, `mouse.up`, `touchscreen.tap`

### Browser & Context
`Browser.launch`, `Browser.connect`, `newPage`, `newContext`, `close`, `isConnected`
`BrowserContext.newPage`, `pages`, `close`, `cookies`, `addCookies`, `clearCookies`, `grantPermissions`, `addInitScript`, `route`

## Workspace

```
crates/
  ferridriver               Core: Browser, Page, Locator, 3 backends
  ferridriver-cli            CLI binary (MCP server + Rust test runner)
  ferridriver-mcp            MCP server library (25 tools, rmcp)
  ferridriver-napi           Node.js/Bun bindings (NAPI-RS)
  ferridriver-test           Test runner: parallel, hooks, expect, reporters
  ferridriver-test-macros    #[ferritest] proc macro
  ferridriver-ct-leptos      Leptos CT adapter (#[component_test] + trunk)
  ferridriver-ct-leptos-macros
  ferridriver-ct-dioxus      Dioxus CT adapter (#[component_test] + dx)
  ferridriver-ct-dioxus-macros
packages/
  ferridriver-test           @ferridriver/test — TS CLI + test API
  ct-core                    @ferridriver/ct-core — Vite plugin, import transform, browser runtime
  ct-react                   @ferridriver/ct-react — React registerSource
  ct-vue                     @ferridriver/ct-vue — Vue registerSource
  ct-svelte                  @ferridriver/ct-svelte — Svelte registerSource
  ct-solid                   @ferridriver/ct-solid — Solid registerSource
examples/
  ct-leptos                  Leptos counter (4 tests)
  ct-leptos-todomvc          Leptos TodoMVC (15 tests)
  ct-dioxus-todomvc          Dioxus TodoMVC (15 tests)
  ct-react                   React TodoMVC (15 tests)
  ct-vue                     Vue TodoMVC (15 tests)
  ct-svelte                  Svelte TodoMVC (15 tests)
  ct-solid                   Solid TodoMVC (15 tests)
```

## Performance

| Metric | Value |
|--------|-------|
| Test runner throughput | **99 tests/sec** (100 tests, 6 workers) |
| vs Playwright Test | **4x faster** (50 tests) |
| CT per test (JS) | 26-36ms |
| CT per test (WASM) | 32-40ms |
| CdpPipe vs Playwright | 1.1x faster per operation |
| WebKit vs Playwright WebKit | 1.3x faster |

## Test Coverage

- 67 Rust integration tests (53 BDD + 14 Page API)
- 250 NAPI tests (Bun, across 3 backends)
- 14 test runner feature tests
- 3 visual screenshot diff tests
- 30 Rust component tests (15 Leptos + 15 Dioxus TodoMVC)
- 60 JS component tests (15 each: React, Vue, Svelte, Solid TodoMVC)
- 3 CT infrastructure tests
- **427+ total tests**

## Building

```bash
# Core library
cargo build -p ferridriver

# MCP server
cargo build -p ferridriver-cli

# NAPI addon
cd crates/ferridriver-napi && bun run build && bun test

# Rust component tests
cargo test -p ct-leptos-todomvc --test todomvc     # requires: cargo install trunk
cargo test -p ct-dioxus-todomvc --test todomvc     # requires: cargo install dioxus-cli

# JS component tests
cd examples/ct-react && bun install && bun run test:ct
cd examples/ct-vue && bun install && bun run test:ct
cd examples/ct-svelte && bun install && bun run test:ct
cd examples/ct-solid && bun install && bun run test:ct

# Or from workspace root
bun install && cd examples/ct-react && bun run test:ct
```

## Requirements

- Rust nightly (edition 2024)
- Chrome/Chromium (auto-detected, or set `CHROMIUM_PATH`)
- macOS 11+ for WebKit backend
- Bun 1.0+ or Node.js 18+ for NAPI and TS test runner
- `trunk` for Leptos CT (`cargo install trunk`)
- `dx` for Dioxus CT (`cargo install dioxus-cli`)

## License

MIT
