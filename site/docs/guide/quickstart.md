# Quickstart

Pick your language. Both Rust and TypeScript drive the same Rust engine.

## Rust

```toml
# Cargo.toml
[dependencies]
ferridriver = "0.3"
tokio       = { version = "1", features = ["full"] }
```

```rust
use ferridriver::browser_type::chromium;
use ferridriver::options::LaunchOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = chromium().launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    page.goto("https://example.com", None).await?;
    page.locator("#email").fill("test@example.com").await?;
    page.locator("button[type=submit]").click().await?;
    page.wait_for_url("/dashboard").await?;

    let png = page.screenshot(Default::default()).await?;
    std::fs::write("home.png", png)?;

    browser.close().await?;
    Ok(())
}
```

`chromium()`, `firefox()`, and `webkit()` are the launch entry points.
Default backend is `CdpPipe` (Chromium over fd 3/4 pipes). Pick a
different one with `BrowserType::chromium_with(&BrowserTypeOptions { transport: ... })`
or `BrowserType::with_backend(...)`.

## TypeScript (Node.js or Bun)

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

```ts
import { chromium } from '@ferridriver/node';

const browser = await chromium().launch();
const page = await browser.newPageWithUrl('https://example.com');

await page.locator('#email').fill('test@example.com');
await page.locator('button[type=submit]').click();
await page.waitForUrl('/dashboard');

const png = await page.screenshot({ fullPage: true });

await browser.close();
```

## Writing tests

Rust test using `ferridriver-test`:

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_homepage(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://example.com", None).await?;
    expect(&page).to_have_title("Example Domain").await?;
}
```

See [Test runner](/test-runner/overview) for harness setup, or
[BDD](/bdd/overview) for Gherkin features with Rust or JavaScript /
TypeScript step bodies.

## Running as an MCP server

```bash
ferridriver mcp                         # stdio (default)
ferridriver mcp --transport http --port 8080
ferridriver mcp --backend webkit --headless
```

See [MCP overview](/mcp/overview) for setup with Claude Code, Claude
Desktop, and Cursor.
