# ferridriver

High-performance browser automation library in Rust with a Playwright-compatible API. Multiple CDP backends, native WebKit, MCP server for AI agents, Node.js/Bun bindings, and a full test runner with component testing for 6 frameworks.

## Architecture

```
ferridriver (core library)
  ├── CdpPipe backend       Chrome via fd 3/4 pipes — fastest, default
  ├── CdpRaw backend        Chrome via WebSocket — connect to running browser
  ├── WebKit backend         macOS WKWebView — native accessibility
  │
  ├── ferridriver-cli        CLI: MCP server (stdio + HTTP)
  ├── ferridriver-napi       Node.js/Bun bindings (NAPI-RS)
  ├── @ferridriver/test      CLI: test runner + component testing (TypeScript)
  │
  ├── ferridriver-test       Test runner core: parallel, hooks, expect, reporters
  │
  ├── @ferridriver/ct-core   JS CT core: Vite plugin, import transform, browser runtime
  ├── @ferridriver/ct-react  React adapter (createRoot/render)
  ├── @ferridriver/ct-vue    Vue adapter (createApp/mount)
  ├── @ferridriver/ct-svelte Svelte adapter (mount, Svelte 4+5)
  └── @ferridriver/ct-solid  Solid adapter (render/dispose)
```

## Installation

### One-line install (Linux/macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/salamaashoush/ferridriver/main/install.sh | bash
```

This installs system dependencies, the `ferridriver` binary, and downloads Chromium.

### Manual install

#### 1. System dependencies

No system library dependencies for building. Video recording (`--video`) requires `ffmpeg` on PATH at runtime.

**Ubuntu/Debian:**
```bash
sudo apt-get install -y pkg-config libclang-dev
# Optional, for --video: sudo apt-get install -y ffmpeg
```

**macOS (Homebrew):**
```bash
brew install pkg-config
# Optional, for --video: brew install ffmpeg
```

#### 2. Install the CLI

**From GitHub releases:**
```bash
# Download the latest release for your platform
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

**From source:**
```bash
cargo install ferridriver-cli
```

#### 3. Install a browser

```bash
# Via the TS CLI
npx @ferridriver/test install chromium
npx @ferridriver/test install --with-deps chromium  # also install system deps (fonts, libs)
```

### npm (Node.js/Bun)

```bash
npm install @ferridriver/test
# or
bun add @ferridriver/test
```

This installs the test runner CLI (`ferridriver-test`) and the `@ferridriver/core` native addon as a dependency. On macOS, it also ships the WebKit host binary.

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
import { Browser } from '@ferridriver/core';

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
npx @ferridriver/test test tests/login.spec.ts --workers 4
```

### E2E Project Setup (Rust)

```
my-project/
  ferridriver.config.toml       # config (optional, auto-discovered)
  tests/
    harness.rs                  # main!() -- one per project
    homepage.rs                 # test modules
    login.rs
    checkout.rs
  Cargo.toml
```

**`tests/harness.rs`** -- entry point, includes all test modules:
```rust
mod homepage;
mod login;
mod checkout;

ferridriver_test::main!();
```

**`tests/homepage.rs`** -- just tests, no boilerplate:
```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_homepage(page: Page) -> Result<(), TestFailure> {
    page.goto("https://example.com", None).await?;
    expect(&page).to_have_title("Example Domain").await?;
    Ok(())
}
```

**`Cargo.toml`:**
```toml
[[test]]
name = "e2e"
path = "tests/harness.rs"
harness = false

[dev-dependencies]
ferridriver-test = { version = "0.2" }
```

**Run:**
```bash
cargo test --test e2e
cargo test --test e2e -- --headed --backend webkit --workers 1
```

### Configuration

All test runners (E2E, CT Rust, CT TypeScript) share the same configuration system.

**`ferridriver.config.toml`** (auto-discovered by walking up from CWD):
```toml
workers = 4
timeout = 30000
retries = 1

[browser]
backend = "cdp-pipe"    # "cdp-pipe", "cdp-raw", "webkit"
headless = true

[browser.viewport]
width = 1280
height = 720
```

**Priority** (lowest to highest):
1. Config file defaults
2. `main!()` / `HarnessConfig` macro arguments
3. Environment variables (`FERRIDRIVER_BACKEND`, `FERRIDRIVER_WORKERS`, `FERRIDRIVER_TIMEOUT`, `FERRIDRIVER_RETRIES`)
4. CLI flags (`--headed`, `--backend`, `--workers`, `--timeout`)

**CLI flags** (after `--` for `cargo test`, direct for `ferridriver-test`):
| Flag | Description |
|---|---|
| `--headed` | Show browser window |
| `--backend <name>` | `cdp-pipe`, `cdp-raw`, `webkit` |
| `--workers <n>` / `-j <n>` | Parallel workers |
| `--retries <n>` | Retry failed tests |
| `--timeout <ms>` | Per-test timeout |
| `--grep <pattern>` / `-g` | Filter tests by name |
| `--tag <name>` | Filter by tag |
| `--list` | List tests without running |
| `--update-snapshots` / `-u` | Update snapshot files |

**Per-test options** (via `#[ferritest]`):
```rust
#[ferritest(retries = 2, timeout = "30s", tag = "smoke")]
async fn flaky_test(page: Page) -> Result<(), TestFailure> { ... }

#[ferritest(skip)]       // skip this test
#[ferritest(slow)]       // mark as slow
#[ferritest(fixme)]      // known broken
```

### Backends

| Backend | Flag | Description |
|---|---|---|
| CDP Pipe | `cdp-pipe` | Chrome via fd 3/4 pipes. Fastest. Default. |
| CDP Raw | `cdp-raw` | Chrome via WebSocket. Connect to running browser. |
| WebKit | `webkit` | Native WKWebView (macOS only). No Chrome needed. |

WebKit uses the system WKWebView -- no browser download, instant startup, native accessibility tree. Headless only (headful mode pending).

### Features

- **Parallel**: N workers x N browsers, MPMC work-stealing dispatch
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

Test UI components in real browsers. JS frameworks use the built-in CT adapters. Rust WASM frameworks (Leptos, Dioxus) use E2E testing with `#[ferritest]` -- build the app with `trunk build` / `dx build`, serve it, and test with the Page API.

### Leptos / Dioxus (E2E)

```rust
use ferridriver_test_macros::ferritest;
use ferridriver_test::expect::expect;

#[ferritest]
async fn counter_increments(page: ferridriver::Page) {
    page.goto("http://localhost:8080", None).await?;
    page.locator("#inc").click().await?;
    expect(&page.locator("#count")).to_have_text("1").await?;
}
```

```bash
# Build the WASM app first, then run E2E tests
cargo install trunk                  # Leptos
trunk build && cargo test -p my-leptos-app

cargo install dioxus-cli             # Dioxus
dx build --platform web && cargo test -p my-dioxus-app
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
npx @ferridriver/test ct --framework react src/todomvc.ct.ts
npx @ferridriver/test ct --framework vue src/todomvc.ct.ts
npx @ferridriver/test ct --framework svelte src/todomvc.ct.ts
npx @ferridriver/test ct --framework solid src/todomvc.ct.ts

# With options
npx @ferridriver/test ct --framework react --headed --backend webkit --workers 1 src/app.ct.ts
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
# stdio (for Claude Code, Cursor, etc.)
ferridriver

# headless mode
ferridriver --headless

# HTTP transport (for remote clients)
ferridriver --transport http --port 8080

# WebKit backend (macOS, no Chrome needed)
ferridriver --backend webkit

# Connect to running Chrome
ferridriver --auto-connect
ferridriver --connect ws://localhost:9222/devtools/browser/...
```

Tools: `connect`, `navigate`, `page`, `click`, `click_at`, `hover`, `fill`, `fill_form`, `type_text`, `press_key`, `drag`, `scroll`, `select_option`, `upload_file`, `snapshot`, `screenshot`, `evaluate`, `wait_for`, `search_page`, `find_elements`, `get_markdown`, `cookies`, `storage`, `emulate`, `diagnostics`

## BDD Framework

58 Gherkin step definitions backed by the Page/Locator API (not raw JS evaluate). All selectors support Playwright engine syntax (`role=`, `text=`, `label=`, etc).

```gherkin
Feature: Login
  Scenario: Successful login
    Given I navigate to "https://app.example.com/login"
    When I fill "label=Email" with "user@example.com"
    And I fill "label=Password" with "secret"
    And I click "role=button[name=Sign in]"
    Then the URL should contain "/dashboard"
    And "role=heading" should have text "Welcome"
```

Available step categories: Navigation, Interaction (click, fill, type, hover, check, focus, blur, scroll), Wait (selector, text, timeout), Assertion (text, visibility, value, attribute, class, state, count), Variable, Cookie, Storage, Screenshot.

## Page API

### Navigation
`goto`, `goBack`, `goForward`, `reload`, `url`, `title`, `content`, `waitForUrl`, `waitForLoadState`, `waitForNavigation`

### Selectors (Playwright-compatible)

All Playwright selector engines are supported:

| Selector | Example |
|---|---|
| CSS | `locator("#submit")`, `locator(".btn.primary")` |
| Role | `getByRole("button", name="Save")` |
| Text | `getByText("Hello")`, `getByText("Hello", exact=true)` |
| Label | `getByLabel("Email")` |
| Placeholder | `getByPlaceholder("Enter name")` |
| Alt text | `getByAltText("Logo")` |
| Title | `getByTitle("Settings")` |
| Test ID | `getByTestId("login-form")` |
| XPath | `locator("xpath=//button")` |
| ID | `locator("id=submit")` |
| Chaining | `locator("css=.form >> role=button")` |
| Filtering | `locator("css=div >> has-text=Keep")` |
| Nth | `locator("css=li >> nth=1")` |

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
  ferridriver-cli            CLI binary (MCP server: stdio + HTTP)
  ferridriver-mcp            MCP server library (25 tools, rmcp)
  ferridriver-napi           Node.js/Bun bindings (NAPI-RS)
  ferridriver-test           Test runner: parallel, hooks, expect, reporters
  ferridriver-test-macros    #[ferritest] proc macro
packages/
  ferridriver-test           @ferridriver/test — TS CLI + test API
  ct-core                    @ferridriver/ct-core — Vite plugin, import transform, browser runtime
  ct-react                   @ferridriver/ct-react — React registerSource
  ct-vue                     @ferridriver/ct-vue — Vue registerSource
  ct-svelte                  @ferridriver/ct-svelte — Svelte registerSource
  ct-solid                   @ferridriver/ct-solid — Solid registerSource
examples/
  ct-leptos-todomvc          Leptos TodoMVC E2E (#[ferritest] + trunk)
  ct-dioxus-todomvc          Dioxus TodoMVC E2E (#[ferritest] + dx)
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
- 60 JS component tests (15 each: React, Vue, Svelte, Solid TodoMVC)
- 3 CT infrastructure tests
- **397+ total tests**

## Building and Testing

```bash
# Run everything: build binary + NAPI, all Rust tests (4 backends), TS tests, BDD features
just test

# Or step by step:
cargo build --bin ferridriver                     # MCP server binary
cd crates/ferridriver-napi && bun run build:debug  # NAPI .node addon
cargo test --workspace                             # Rust tests
cd crates/ferridriver-napi && bun test             # NAPI/TS tests
```

## Requirements

- Rust nightly (edition 2024)
- Chrome/Chromium (auto-detected, or set `CHROMIUM_PATH`)
- macOS 11+ for WebKit backend
- Bun 1.0+ or Node.js 18+ for NAPI and TS test runner
- `ffmpeg` on PATH for `--video` recording (optional, runtime only)
- `trunk` for Leptos CT (`cargo install trunk`)
- `dx` for Dioxus CT (`cargo install dioxus-cli`)

## License

MIT
