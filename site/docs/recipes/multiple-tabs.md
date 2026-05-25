# Multiple tabs and windows

ferridriver models every tab and popup as a `Page` on the same
`BrowserContext`. Cookies and storage are shared between them.

## Open a new tab

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn checkout_in_new_tab(ctx: TestContext) {
    let page = ctx.page().await?;
    let context = ctx.context().await?;

    // Open a fresh tab in the same context (shared cookies/storage)
    let second = context.new_page().await?;
    second.goto("https://app.example.com/cart", None).await?;
    expect(&second.locator(".cart-total")).to_have_text("$42.00").await?;

    second.close(None).await?;
}
```

## Wait for a popup triggered by the page

```rust
let popup = page
    .wait_for_event("popup", 10_000)
    .await?
    .into_page()
    .expect("popup event payload");
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
        p.close(None).await?;
    }
}
```

## OAuth popup flow

```rust
#[ferritest]
async fn oauth_login(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://app.example.com/login", None).await?;

    page.locator("button.sign-in-with-github").click().await?;
    let popup = page
        .wait_for_event("popup", 10_000)
        .await?
        .into_page()
        .expect("popup");

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

const [popup] = await Promise.all([
  page.waitForEvent('popup'),
  page.locator('button.open-popup').click(),
]);

await popup.waitForLoadState('load');
```
