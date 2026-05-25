# Network mocking

`page.route(pattern, handler)` intercepts requests matching a URL glob.
The handler chooses `fulfill` (mock a response), `continue_route`
(forward with modifications), or `abort` (cancel with an error code).

## Mock a JSON response

```rust
use ferridriver_test::prelude::*;
use ferridriver::route::{Route, FulfillResponse};
use std::sync::Arc;

#[ferritest]
async fn mocks_user_list(ctx: TestContext) {
    let page = ctx.page().await?;

    page.route("**/api/users", Arc::new(|route: Route| async move {
        route.fulfill(FulfillResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: br#"[{"id":1,"name":"Ada"},{"id":2,"name":"Grace"}]"#.to_vec(),
            content_type: Some("application/json".into()),
        }).await.ok();
    })).await?;

    page.goto("https://app.example.com/users", None).await?;
    expect(&page.locator(".user-row")).to_have_count(2).await?;
}
```

## Block third-party trackers

```rust
page.route("**/{google-analytics,segment,mixpanel}.com/**",
    Arc::new(|route: Route| async move {
        route.abort("blockedbyclient").await.ok();
    })
).await?;
```

## Modify a request

```rust
use ferridriver::route::ContinueOverrides;

page.route("**/api/**", Arc::new(|route: Route| async move {
    let mut headers = route.request().headers.clone();
    headers.insert("x-test-run".into(), "ci-12345".into());
    route.continue_route(ContinueOverrides {
        url: None,
        method: None,
        headers: Some(headers),
        post_data: None,
    }).await.ok();
})).await?;
```

## Wait for a specific response

```rust
let response = page.wait_for_response("**/api/checkout", 30_000).await?;
assert_eq!(response.status(), 200);
let body: serde_json::Value = response.json().await?;
assert_eq!(body["order_id"], "abc-123");
```

## TypeScript

```ts
await page.route('**/api/users', async (route) => {
  await route.fulfill({
    status: 200,
    contentType: 'application/json',
    body: JSON.stringify([
      { id: 1, name: 'Ada' },
      { id: 2, name: 'Grace' },
    ]),
  });
});

await page.goto('https://app.example.com/users');
```

## Context-wide routing

`BrowserContext::route` applies to every page in the context — useful
when you have multi-tab flows:

```rust
let context = ctx.context().await?;
context.route("**/api/**", Arc::new(handler)).await?;
```

## HAR recording

Capture all network traffic to a HAR file for later replay or
inspection:

```toml
# ferridriver.toml
[test.browser.useOptions.recordHar]
path = "test-results/network.har"
content = "embed"
```

The `bidi` backend does not support HAR recording — it returns
`FerriError::Unsupported`. Use `cdp-pipe` / `cdp-raw` / `webkit` for HAR
flows.

## Removing routes

```rust
page.unroute("**/api/users").await?;
```

Or set up the route inside a test fixture so teardown removes it
automatically when the context closes.
