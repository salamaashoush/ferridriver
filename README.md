# ferridriver

Browser automation for Rust projects. Playwright-compatible API, native Rust engine, a parallel test runner, a BDD framework that runs JavaScript/TypeScript step files natively (no Node/Bun), a core browser binding for Node/Bun, and an MCP server for AI agents. Four backends (CDP pipe / CDP WebSocket / WKWebView / Firefox via BiDi), one API.

## Architecture

```
ferridriver (core library)
  ├── CdpPipe backend        Chrome via fd 3/4 pipes — fastest, default
  ├── CdpRaw backend         Chrome via WebSocket — connect to running browser
  ├── WebKit backend         macOS WKWebView — native accessibility
  ├── Bidi backend           Firefox via WebDriver BiDi
  │
  ├── ferridriver-cli         CLI binary: mcp · bdd · test · run · install
  ├── ferridriver-mcp         MCP server library (scripting-focused, 9 tools, rmcp)
  ├── ferridriver-script      QuickJS engine (run_script + JS/TS BDD step bodies)
  ├── ferridriver-node        Core browser binding (NAPI-RS) → @ferridriver/node
  │
  ├── ferridriver-test        Test runner core: parallel, hooks, expect, reporters
  ├── ferridriver-test-macros Proc macros: #[ferritest], #[ferritest_each], hooks
  │
  ├── ferridriver-bdd         BDD framework: Gherkin parser, steps, translator
  └── ferridriver-bdd-macros  Proc macros: #[given], #[when], #[then], #[step]
```

`@ferridriver/node` is the browser API for JavaScript: `Browser` /
`BrowserContext` / `Page` / `Frame` / `Locator` / `ElementHandle` /
`Mouse` / `Keyboard` / network / dialog. For testing, write Rust
`#[ferritest]` tests or Gherkin features with JS/TS step bodies via
`ferridriver bdd`.

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
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

**From source:**
```bash
cargo install ferridriver-cli
```

#### 3. Install a browser

```bash
ferridriver install chromium
ferridriver install --with-deps chromium   # also install system libs
ferridriver install firefox chromium-headless-shell
```

### npm (Node.js/Bun) — core browser binding only

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

On macOS this also ships the WebKit host binary.

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
import { Browser } from '@ferridriver/node';

const browser = await Browser.launch();
const page = await browser.newPageWithUrl('https://example.com');
await page.locator('h1').click();
console.log(await page.locator('h1').textContent());
await browser.close();
```

## Test Runner (Rust)

Parallel test execution with auto-retrying assertions.

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

### E2E Project Setup

```
my-project/
  ferridriver.config.toml       # config (optional, auto-discovered)
  tests/
    harness.rs                  # main!() -- one per project
    homepage.rs                 # test modules
    login.rs
  Cargo.toml
```

**`tests/harness.rs`** -- entry point, includes all test modules:
```rust
mod homepage;
mod login;

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
ferridriver-test = "0.1"
```

**Run:**
```bash
cargo test --test e2e
cargo test --test e2e -- --headed --backend webkit --workers 1
```

### Configuration

**`ferridriver.config.toml`** (auto-discovered by walking up from CWD):
```toml
workers = 4
timeout = 30000
retries = 1

[browser]
backend = "cdp-pipe"    # "cdp-pipe", "cdp-raw", "webkit", "bidi"
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

**CLI flags** (after `--` for `cargo test`):
| Flag | Description |
|---|---|
| `--headed` | Show browser window |
| `--backend <name>` | `cdp-pipe`, `cdp-raw`, `webkit`, `bidi` |
| `--browser <name>` | `chromium`, `firefox`, `webkit` (sets default backend) |
| `--workers <n>` / `-j <n>` | Parallel workers |
| `--retries <n>` | Retry failed tests |
| `--timeout <ms>` | Per-test timeout |
| `--grep <pattern>` / `-g` | Filter tests by name |
| `--tag <name>` | Filter by tag |
| `--shard <cur>/<total>` | Shard selection for CI |
| `--list` | List tests without running |
| `--update-snapshots` | Update snapshot files |
| `--last-failed` | Re-run only previously failed tests |
| `--forbid-only` | Fail if any `test.only()` is present |

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
| CDP Raw | `cdp-raw` | Chrome via WebSocket. Connect to a running browser. |
| WebKit | `webkit` | Native WKWebView (macOS only). No Chrome needed. |
| Bidi | `bidi` | Firefox via WebDriver BiDi. |

WebKit uses the system WKWebView — no browser download, instant startup, native accessibility tree. Headless only (headful mode pending).

### Expect Matchers

`ferridriver-test::expect` ships **38 matchers** with auto-retry. All
polling happens in Rust.

Visibility: `to_be_visible`, `to_be_hidden`, `to_be_attached`, `to_be_in_viewport`
State: `to_be_enabled`, `to_be_disabled`, `to_be_checked`, `to_be_editable`, `to_be_focused`, `to_be_empty`
Text: `to_have_text`, `to_contain_text`, `to_have_texts`, `to_contain_texts`
Value: `to_have_value`, `to_have_values`
Attributes: `to_have_attribute`, `to_have_class`, `to_contain_class`, `to_have_css`, `to_have_id`, `to_have_role`
A11y: `to_have_accessible_name`, `to_have_accessible_description`, `to_have_accessible_error_message`, `to_match_aria_snapshot`
Snapshots: `to_match_snapshot`, `to_have_screenshot`
Other: `to_have_js_property`, `to_have_count`
Page: `to_have_title`, `to_contain_title`, `to_have_url`, `to_contain_url`
Poll / satisfy: `expect_poll`, `to_equal`, `to_satisfy`, `to_pass`, `to_pass_with_options`
Modifiers: `.not()`, `.with_timeout()`, `.soft()`, `.with_message()`

## BDD Framework

Gherkin step definitions backed by the Page/Locator API. Step bodies can be
Rust (`#[given]`/`#[when]`/`#[then]`) or JavaScript/TypeScript — TS/JS step
files are bundled with rolldown, compiled to QuickJS bytecode once, and run
on the shared `ferridriver-script` engine through the core `TestRunner`.
There is no Node or Bun in the run path.

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

```bash
# Rust steps only
ferridriver bdd tests/features/

# JavaScript / TypeScript step files (cucumber-js shaped)
ferridriver bdd --steps 'steps/**/*.{js,ts}' tests/features/
```

JS/TS step files keep the cucumber-js surface: `Given`/`When`/`Then`/
`Before`/`After`/`BeforeAll`/`AfterAll`/`defineParameterType`/
`setWorldConstructor`/`setDefaultTimeout`, `this` is the World with
`attach`/`log`/`parameters`, `DataTable` exposes
`raw`/`rows`/`hashes`/`rowsHash`/`transpose`, and returning
`'pending'`/`'skipped'` works. Run with the single `ferridriver` binary —
no `package.json`, no `node_modules`.

Available step categories: Navigation, Interaction (click, fill, type, hover, check, focus, blur, scroll), Wait, Assertion (text, visibility, value, attribute, class, state, count, aria), Keyboard, Mouse, Screenshot, Variable, Storage, Cookie, JavaScript, Dialog, Frame, Window, File, Network (route / fulfill / continue / abort), API request, Emulation.

## MCP Server

Scripting-focused MCP server for AI agent browser automation. Works with Claude, Cursor, Claude Code, or any MCP client.

```bash
# stdio (for Claude Code, Cursor, etc.)
ferridriver mcp

# headless mode
ferridriver mcp --headless

# HTTP transport (for remote clients)
ferridriver mcp --transport http --port 8080

# WebKit backend (macOS, no Chrome needed)
ferridriver mcp --backend webkit

# Connect to running Chrome
ferridriver mcp --auto-connect chrome
ferridriver mcp --connect ws://localhost:9222/devtools/browser/...
```

**Nine tools:** `connect`, `navigate`, `page` (session bootstrap) · `snapshot`, `screenshot`, `evaluate`, `search_page`, `diagnostics` (observation) · `run_script` (action).

`run_script` runs sandboxed JavaScript against the live session with full Page / Locator / BrowserContext / HttpClient bindings over the ferridriver core. One script can navigate, fill forms, click, assert, and make HTTP calls in a single LLM turn — no per-action round-trips.

```js
// Example run_script payload
await page.goto(args[0]);
await page.getByLabel('Email').fill(args[1]);
await page.getByLabel('Password').fill(args[2]);
await page.getByRole('button', { name: 'Sign in' }).click();
await page.waitForSelector('[data-testid="dashboard"]');
return { title: await page.title(), cookies: await context.cookies() };
```

Globals available inside a script: `page`, `context`, `request`, `args` (bound, not interpolated — prompt-injection safe), `vars` (session-level key/value store), `console.*` (captured with size limits), `fs` (scoped read/write under a configured `script_root`). Error responses include stack, line, column, and a source snippet so the model can self-correct.

See [`site/docs/mcp/tools.md`](./site/docs/mcp/tools.md) for the full script API.

## Page API

### Navigation
`goto`, `goBack`, `goForward`, `reload`, `url`, `title`, `content`, `waitForUrl`, `waitForLoadState`, `waitForNavigation`

### Selectors (Playwright-compatible)

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
  ferridriver                 Core: Browser, Page, Locator, 4 backends
  ferridriver-cli             CLI binary: mcp · bdd · test · run · install
  ferridriver-mcp             MCP server library (scripting-focused, 9 tools, rmcp)
  ferridriver-script          QuickJS engine (run_script + JS/TS BDD step bodies)
  ferridriver-node            Core browser binding (NAPI-RS) → @ferridriver/node
  ferridriver-test            Test runner: parallel, hooks, expect, reporters
  ferridriver-test-macros     #[ferritest], #[ferritest_each], hook macros
  ferridriver-bdd             BDD framework: Gherkin parser, steps, translator
  ferridriver-bdd-macros      #[given], #[when], #[then], #[step], #[before], #[after]
examples/
  bdd-example                 Rust + JS/TS BDD test suite (feature files + steps)
site/                         Documentation site (rspress)
```

## Building and Testing

```bash
# Build the binary, run all Rust tests (4 backends), then the BDD suite
just test

# Or step by step:
cargo build --bin ferridriver
cargo test --workspace
cargo run --bin ferridriver -- bdd tests/features/

# Core-binding bun tests for @ferridriver/node (optional):
cd crates/ferridriver-node && bun run build:debug && bun test
```

## Requirements

- Rust stable 1.91+ (edition 2024) — see `rust-toolchain.toml`
- Chrome / Chromium (`ferridriver install chromium`, auto-detected, or set `CHROMIUM_PATH`)
- macOS 11+ for the WebKit backend
- Firefox with WebDriver BiDi for the `bidi` backend
- Bun 1.0+ or Node.js 18+ only if you build/consume the `@ferridriver/node` addon
- `ffmpeg` on PATH for `--video` recording (optional, runtime only)

## License

MIT
