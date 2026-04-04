# ferridriver

High-performance browser automation library in Rust. Playwright-compatible API across multiple backends, optimized for speed.

## Architecture

```
ferridriver (Rust library)
  |-- CdpPipe backend    (Chrome via fd 3/4 pipes -- fastest)
  |-- CdpRaw backend     (Chrome via WebSocket -- fully parallel)
  |-- WebKit backend      (macOS WKWebView -- native NSAccessibility)
  |
  |-- ferridriver-cli     (MCP server for AI agent automation)
  |-- ferridriver-napi    (Node.js/Bun bindings via NAPI-RS)
```

### Backends

| Backend | Transport | Use case |
|---------|-----------|----------|
| **CdpPipe** | Unix pipes (fd 3/4) | Default. Lowest latency, no port discovery. |
| **CdpRaw** | WebSocket | Connect to running Chrome, full parallel multi-page. |
| **WebKit** | Binary IPC to WKWebView subprocess | macOS only. Native accessibility, native mouse events. |

## Quick Start (Rust)

```rust
use ferridriver::{Browser, Page};
use ferridriver::options::{LaunchOptions, RoleOptions, GotoOptions};

#[tokio::main]
async fn main() -> Result<(), String> {
    let browser = Browser::launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    // Navigate
    page.goto("https://example.com", None).await?;

    // Locators (Playwright-style)
    page.get_by_role("link", RoleOptions { name: Some("More".into()), ..Default::default() })
        .click().await?;

    // Fill forms
    page.locator("#email").fill("test@example.com").await?;
    page.locator("#password").fill("secret").await?;
    page.locator("button[type=submit]").click().await?;

    // Wait for navigation
    page.wait_for_url("/dashboard").await?;

    // Extract content
    let title = page.title().await?;
    let md = page.markdown().await?;

    // Screenshot
    let png = page.screenshot(Default::default()).await?;

    // Accessibility snapshot (LLM-optimized)
    let snap = page.snapshot_for_ai(Default::default()).await?;
    println!("{}", snap.full);

    browser.close().await?;
    Ok(())
}
```

## Quick Start (Node.js/Bun)

```typescript
import { Browser } from 'ferridriver-napi';

const browser = await Browser.launch();
const page = await browser.page();

await page.goto('https://example.com');
await page.locator('h1').click();
const text = await page.locator('h1').textContent();
console.log(text);

// Event listeners
const id = page.on('response', (data) => {
  console.log(`${data.status} ${data.url}`);
});
page.off(id); // remove listener

await browser.close();
```

## API Reference

### Page

#### Navigation
- `goto(url, opts?)` -- navigate with optional `{ waitUntil, timeout }`
- `go_back(opts?)` / `go_forward(opts?)` / `reload(opts?)`
- `url()` / `title()` / `content()`
- `wait_for_url(pattern)` / `wait_for_load_state(state?)`
- `wait_for_navigation(timeout?)`

#### Locators
- `locator(selector)` -- CSS, XPath, or rich selectors (`role=`, `text=`, `testid=`)
- `get_by_role(role, opts?)` / `get_by_text(text, opts?)` / `get_by_label(text, opts?)`
- `get_by_placeholder(text, opts?)` / `get_by_alt_text(text, opts?)` / `get_by_title(text, opts?)`
- `get_by_test_id(id)`

#### Locator Actions
- `click()` / `dblclick()` / `right_click()` / `tap()`
- `fill(value)` / `clear()` / `type_text(text)` / `press(key)` / `press_sequentially(text)`
- `hover()` / `focus()` / `blur()` / `scroll_into_view()`
- `check()` / `uncheck()` / `set_checked(bool)`
- `select_option(value)` / `set_input_files(paths)` / `select_text()`
- `drag_to(target_locator)` / `dispatch_event(event_type)`

#### Locator Queries
- `text_content()` / `inner_text()` / `inner_html()` / `input_value()`
- `get_attribute(name)` / `bounding_box()`
- `is_visible()` / `is_hidden()` / `is_enabled()` / `is_disabled()` / `is_checked()` / `is_editable()` / `is_attached()`
- `count()` / `all()` / `first()` / `last()` / `nth(index)`
- `all_text_contents()` / `all_inner_texts()`
- `evaluate(expression)` / `evaluate_all(expression)`
- `or(other)` / `and(other)` / `filter(opts)`

#### Screenshots & Content
- `screenshot(opts?)` / `screenshot_element(selector)` / `pdf(landscape, print_bg)`
- `markdown()` / `set_content(html)`
- `add_script_tag(url?, content?, type?)` / `add_style_tag(url?, content?)`

#### Accessibility Snapshot
- `snapshot_for_ai(opts?)` -- LLM-optimized accessibility tree with optional depth limiting and incremental change tracking

#### Network Interception
- `route(pattern, handler)` -- intercept requests matching a glob pattern
- `unroute(pattern)` -- remove route handlers
- Supports `fulfill` (mock response), `continue` (modify request), `abort` (block request)
- CDP: native Fetch domain. WebKit: JS fetch/XHR monkey-patching via WKScriptMessageHandlerWithReply.

#### Events
- `on(event, callback)` / `once(event, callback)` / `off(listener_id)` / `remove_all_listeners()`
- `wait_for_event(name, timeout?)` / `wait_for_response(url_pattern, timeout?)`
- `wait_for_request(url_pattern, timeout?)` / `wait_for_download(url_pattern?, timeout?)`
- `expect_navigation(timeout?)` / `expect_response(url_pattern, timeout?)` / `expect_request(url_pattern, timeout?)`
- Events: `console`, `request`, `response`, `dialog`, `download`, `load`, `domcontentloaded`, `close`, `pageerror`, `frameattached`, `framedetached`, `framenavigated`

#### Dialog Handling
- `set_dialog_handler(handler)` -- configure how JS dialogs (alert/confirm/prompt) are handled
- Default: auto-accept alerts/confirms, accept prompts with default value

#### JavaScript Bridge
- `evaluate(expression)` / `evaluate_str(expression)`
- `add_init_script(source)` / `remove_init_script(id)` -- inject JS before page scripts on every navigation
- `expose_function(name, callback)` / `remove_exposed_function(name)` -- bridge Rust functions to page JS

#### Emulation
- `set_viewport_size(w, h)` / `set_viewport(config)` / `viewport_size()`
- `set_user_agent(ua)` / `set_locale(locale)` / `set_timezone(tz)`
- `set_geolocation(lat, lng, accuracy)` / `set_network_state(offline, latency, dl, ul)`
- `emulate_media(opts)` / `set_javascript_enabled(bool)`
- `set_extra_http_headers(headers)` / `grant_permissions(perms, origin?)`

#### Cookies & Storage
- `cookies()` / `set_cookie(cookie)` / `delete_cookie(name, domain?)` / `clear_cookies()`
- `storage_state()` / `set_storage_state(json)` -- save/restore full session state

#### Input Devices
- `page.keyboard().press(key)` / `page.keyboard().type(text)`
- `page.mouse().click(x, y, opts?)` / `page.mouse().move(x, y)` / `page.mouse().wheel(dx, dy)`
- `page.mouse().down(x, y, button?)` / `page.mouse().up(x, y, button?)`
- `page.touchscreen().tap(x, y)`

#### Frames
- `main_frame()` / `frames()` / `frame(name_or_url)`
- Frame has its own `evaluate()`, `locator()`, `get_by_*()`, `content()`, `set_content()`, `add_script_tag()`, `add_style_tag()`

#### Lifecycle
- `close()` / `is_closed()` / `bring_to_front()`

### Browser
- `Browser::launch(opts)` / `Browser::connect(ws_url)`
- `new_page()` / `new_page_with_url(url)` / `page()`
- `new_context()` / `default_context()` / `contexts()`
- `close()` / `is_connected()` / `version()`

### BrowserContext
- `new_page()` / `pages()` / `close()`
- `cookies()` / `add_cookies(cookies)` / `clear_cookies()` / `delete_cookie(name, domain?)`
- `grant_permissions(perms, origin?)` / `clear_permissions()`
- `set_geolocation(lat, lng, accuracy)` / `set_extra_http_headers(headers)` / `set_offline(bool)`
- `add_init_script(source)` / `route(pattern, handler)` / `unroute(pattern)`

## MCP Server (ferridriver-cli)

25 tools for AI agent browser automation. Install as an MCP server for Claude, Cursor, or any MCP-compatible client.

```bash
cargo install ferridriver-cli
```

Tools: `navigate`, `page`, `click`, `click_at`, `hover`, `fill`, `fill_form`, `type_text`, `press_key`, `drag`, `scroll`, `select_option`, `upload_file`, `snapshot`, `screenshot`, `evaluate`, `wait_for`, `search_page`, `get_markdown`, `cookies`, `storage`, `emulate`, `diagnostics`, `list_steps`, `run_scenario`

## BDD Framework

58 Gherkin step definitions for browser automation testing.

```gherkin
Feature: Login
  Scenario: Successful login
    Given I navigate to "https://app.example.com/login"
    When I fill "#email" with "user@example.com"
    And I fill "#password" with "secret"
    And I click "#submit"
    Then the URL should contain "/dashboard"
    And the title should contain "Dashboard"
    And "#welcome" should contain text "Welcome"
```

Step categories: Navigation (5), Interaction (14), Wait (6), Assertion (24), Variable (7), Cookie (4), Storage (3), Screenshot (3), JavaScript (1).

## Test Runner (ferridriver-test)

Playwright-compatible test runner with parallel execution, auto-retrying assertions, and rich reporting.

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn login_flow(page: Page) {
    page.goto("https://app.example.com/login", None).await.unwrap();
    page.locator("#email").fill("user@example.com").await.unwrap();
    page.locator("button[type=submit]").click().await.unwrap();
    expect(&page).to_have_url("dashboard").await.unwrap();
}
```

- **Parallel execution**: N workers × N browsers, MPMC work-stealing dispatch
- **Hooks**: beforeAll/afterAll, beforeEach/afterEach
- **Serial mode**: `SuiteMode::Serial` — run in order, skip on failure
- **32 expect matchers**: visibility, text, value, attributes, count, accessibility, snapshots
- **Visual screenshot diffing**: pixel-level PNG comparison with threshold and diff image
- **Reporters**: Terminal, JUnit XML, JSON, HTML (self-contained)
- **95+ tests/sec** (3.7x faster than Playwright Test)

See [crates/ferridriver-test/README.md](crates/ferridriver-test/README.md) for full API.

## Component Testing

Test UI components in real browsers with `cargo test` or `bun test`. Supports both Rust WASM frameworks and JS frameworks.

### Rust Frameworks (Leptos, Dioxus)

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

- `trunk build` / `dx build` (cached) → static serve → parallel test runner
- Custom harness: one browser, fresh page per test, inventory-based discovery
- **15 TodoMVC tests in 531ms** (Leptos), **622ms** (Dioxus)
- **500 tests in 10.1s** (49.5 tests/sec)

### JS Frameworks (React, Vue, Svelte)

```typescript
import { test, expect } from '@ferridriver/ct-react';
import Counter from './Counter';

test('increments', async ({ mount, page }) => {
  await mount(Counter, { props: { initial: 0 } });
  await page.locator('#inc').click();
  await expect(page.locator('#count')).toHaveText('1');
});
```

- Import transform → Vite build → component registry → mount via `page.evaluate()`
- Framework adapters: `@ferridriver/ct-react`, `@ferridriver/ct-vue`, `@ferridriver/ct-svelte`
- Same expect API as Playwright CT

### Workspace Layout

```
crates/
  ferridriver              Core library: Browser, Page, Locator, backends
  ferridriver-cli          MCP server binary
  ferridriver-mcp          MCP server library (25 tools)
  ferridriver-napi         Node.js/Bun bindings (NAPI-RS)
  ferridriver-test         Test runner: parallel, hooks, expect, reporters
  ferridriver-test-macros  #[ferritest] proc macro
  ferridriver-ct-leptos    Leptos component testing adapter
  ferridriver-ct-dioxus    Dioxus component testing adapter
packages/
  ct-core                  JS CT core: import transform, Vite plugin, browser runtime
  ct-react                 React adapter: registerSource + test API
  ct-vue                   Vue adapter
  ct-svelte                Svelte adapter
examples/
  ct-leptos-todomvc        Leptos TodoMVC with 15 component tests
  ct-dioxus-todomvc        Dioxus TodoMVC with 15 component tests
  ct-react                 React counter with CT tests
```

## Performance

- CdpPipe: 1.1x faster than Playwright on equivalent operations
- WebKit: 1.3x faster than Playwright's patched WebKit
- Test runner: 95+ tests/sec (3.7x faster than Playwright Test)
- Component testing: 49.5 tests/sec on real WASM apps
- Single CDP call per element interaction (scroll + getBoundingClientRect + dispatch in one evaluate)
- FxHashMap for all internal maps (faster than std HashMap)
- Zero-copy screenshot transfer via shared memory on WebKit

## Test Coverage

- 67 Rust integration tests (53 BDD + 14 Page API)
- 250 NAPI tests (Bun, across all 3 backends)
- 14 test runner feature tests (hooks, serial, expected failures, soft assertions, snapshots)
- 3 visual screenshot diff tests
- 30 component tests (15 Leptos TodoMVC + 15 Dioxus TodoMVC)
- 3 CT infrastructure tests
- **367+ total tests**

## Building

```bash
# Rust library
cargo build --package ferridriver

# MCP server
cargo build --package ferridriver-cli

# NAPI addon (requires Node.js or Bun)
cd crates/ferridriver-napi
bun run build
bun test

# Run test runner benchmarks
cargo test --package ferridriver-test --test bench_runner -- --nocapture

# Run component tests (requires trunk)
cargo test -p ct-leptos-todomvc --test todomvc

# Run component tests (requires dx)
cargo test -p ct-dioxus-todomvc --test todomvc
```

## Requirements

- Rust nightly (edition 2024)
- Chrome/Chromium (auto-detected, or set `CHROMIUM_PATH`)
- macOS 11+ for WebKit backend
- Node.js 18+ or Bun 1.0+ for NAPI bindings
- `trunk` for Leptos component testing (`cargo install trunk`)
- `dx` for Dioxus component testing (`cargo install dioxus-cli`)

## License

MIT
