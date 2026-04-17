# Quickstart

Pick your language.

## Rust

```toml
# Cargo.toml
[dependencies]
ferridriver = "0.1"
tokio = { version = "1", features = ["full"] }
```

```rust
use ferridriver::{Browser, Page};
use ferridriver::options::LaunchOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = Browser::launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    page.goto("https://example.com", None).await?;
    page.locator("#email").fill("test@example.com").await?;
    page.locator("button[type=submit]").click().await?;
    page.wait_for_url("/dashboard").await?;

    let png = page.screenshot(Default::default()).await?;
    browser.close().await?;
    Ok(())
}
```

## TypeScript (Node.js or Bun)

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

```ts
import { Browser } from '@ferridriver/node';

const browser = await Browser.launch();
const page = await browser.newPageWithUrl('https://example.com');

await page.locator('#email').fill('test@example.com');
await page.locator('button[type=submit]').click();
await page.waitForUrl('/dashboard');

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

See the [Test runner](/test-runner/overview) guide for setup, or [Component testing](/component-testing/overview) for React / Vue / Svelte / Solid.
