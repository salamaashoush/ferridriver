# ferridriver

High-performance browser automation library in Rust. Playwright-compatible API with four backends:

- **CdpPipe** — Chrome DevTools Protocol over Unix pipes (fastest, default)
- **CdpRaw** — Chrome DevTools Protocol over WebSocket (can attach to a running Chrome)
- **WebKit** — Native WKWebView on macOS (native accessibility tree, native mouse events)
- **Bidi** — WebDriver BiDi protocol (Firefox)

## Usage

```rust
use ferridriver::{Browser, Page};
use ferridriver::options::LaunchOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = Browser::launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    page.goto("https://example.com", None).await?;
    let title = page.title().await?;

    page.locator("#search").fill("rust").await?;
    page.locator("button[type=submit]").click().await?;
    page.wait_for_load_state(Some("networkidle")).await?;

    browser.close().await?;
    Ok(())
}
```

## Features

- Playwright-compatible `Page`, `Locator`, `Frame`, `BrowserContext` APIs
- Network interception (`route` / `unroute`) with `fulfill` / `continue` / `abort`
- Dialog handling (configurable auto-accept or custom handler)
- Init script injection (runs before page scripts on every navigation)
- Expose Rust functions to page JavaScript
- Accessibility snapshots optimized for LLM consumption
- Cookie and localStorage state save/restore
- 144 built-in BDD step definitions (via `ferridriver-bdd`)

See the [workspace README](../../README.md) for the full Page API reference and the test-runner / MCP / BDD stories.
