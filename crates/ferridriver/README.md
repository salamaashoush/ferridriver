# ferridriver

High-performance browser automation library in Rust. Playwright-compatible API with three backends:

- **CdpPipe** -- Chrome DevTools Protocol over Unix pipes (fastest, default)
- **CdpRaw** -- Chrome DevTools Protocol over WebSocket (connect to running Chrome)
- **WebKit** -- Native WKWebView on macOS (native accessibility tree, native mouse events)

## Usage

```rust
use ferridriver::{Browser, Page};
use ferridriver::options::LaunchOptions;

let browser = Browser::launch(LaunchOptions::default()).await?;
let page = browser.page().await?;

page.goto("https://example.com", None).await?;
let title = page.title().await?;

page.locator("#search").fill("rust").await?;
page.locator("button[type=submit]").click().await?;
page.wait_for_load_state(Some("networkidle")).await?;

browser.close().await?;
```

## Features

- Playwright-compatible Page, Locator, Frame, BrowserContext APIs
- Network interception (route/unroute) with fulfill/continue/abort
- Dialog handling (configurable auto-accept or custom handler)
- Init script injection (runs before page scripts on every navigation)
- Expose Rust functions to page JavaScript
- Accessibility snapshots optimized for LLM consumption
- Cookie and localStorage state save/restore
- BDD step definitions (58 Gherkin steps for browser automation)

See the [workspace README](../../README.md) for full API reference.
