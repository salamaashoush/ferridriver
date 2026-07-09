# Multiple tabs and windows

ferridriver models every tab and popup as a `Page` on the same
`BrowserContext`. Cookies and storage are shared between them.

## Open a new tab

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn checkout_in_new_tab(context: Arc<BrowserContext>) {
    // Open a fresh tab in the same context (shared cookies/storage)
    let second = context.new_page().await?;
    second.goto("https://app.example.com/cart").await?;
    expect(&second.locator(".cart-total")).to_have_text("$42.00").await?;

    second.close().await?;
}
```

## Wait for a tab the page opens

There is no popup event — when the page opens a new tab (`window.open`,
`target="_blank"`), poll `context.pages()` until a new `Page` appears.

```rust
let before = context.pages().await?.len();
page.locator("a.open-report").click().await?;

let popup = loop {
    let pages = context.pages().await?;
    if pages.len() > before {
        break pages.into_iter().last().expect("new page");
    }
};
popup.wait_for_load_state(Some("load")).await?;
expect(&popup).to_have_url("https://oauth.example.com/").await?;
```

## Iterate all pages in a context

```rust
let pages = context.pages().await?;
for p in pages {
    println!("{} — {}", p.url(), p.title().await?);
}
```

## Close a specific tab

```rust
let pages = context.pages().await?;
for p in pages {
    if p.url().contains("/old-flow") {
        p.close().await?;
    }
}
```

## OAuth popup flow

```rust
#[ferritest]
async fn oauth_login(page: Arc<Page>, context: Arc<BrowserContext>) {
    page.goto("https://app.example.com/login").await?;

    let before = context.pages().await?.len();
    page.locator("button.sign-in-with-github").click().await?;

    // The page opens the GitHub OAuth tab; grab it once it appears.
    let popup = loop {
        let pages = context.pages().await?;
        if pages.len() > before {
            break pages.into_iter().last().expect("oauth tab");
        }
    };

    popup.locator("#login_field").fill("ada").await?;
    popup.locator("#password").fill("secret").await?;
    popup.locator("input[type=submit]").click().await?;
    popup.locator("button[name=authorize]").click().await?;
    // popup closes itself after redirect

    expect(&page).to_have_url("/dashboard").await?;
}
```

## MCP server: switching tabs

The MCP `page` tool manages the active page within a session:

```jsonc
{ "tool": "page", "arguments": { "action": "list" } }
{ "tool": "page", "arguments": { "action": "select", "page_index": 1 } }
{ "tool": "page", "arguments": { "action": "new", "url": "https://example.com" } }
{ "tool": "page", "arguments": { "action": "close", "page_index": 0 } }
```

After `page(select)` or `page(new)`, **refs from the previous
`snapshot` become invalid** — re-snapshot before clicking.

## TypeScript

```ts
const second = await context.newPage();
await second.goto('https://app.example.com/cart');

// No 'popup' event — open the tab explicitly, or poll context.pages()
// after the click that triggers window.open / target="_blank".
const before = (await context.pages()).length;
await page.locator('button.open-popup').click();

let popup;
do {
  const pages = await context.pages();
  popup = pages.length > before ? pages[pages.length - 1] : undefined;
} while (!popup);

await popup.waitForLoadState('load');
```
