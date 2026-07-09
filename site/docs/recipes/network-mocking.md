# Network mocking

`page.route(pattern, handler)` intercepts requests matching a URL glob.
The handler chooses `fulfill` (mock a response), `continue_route`
(forward with modifications), or `abort` (cancel with an error code).

## Mock a JSON response

```rust
use ferridriver_test::prelude::*;
use ferridriver::route::{Route, RouteHandler, FulfillResponse};
use ferridriver::url_matcher::UrlMatcher;
use std::sync::Arc;

#[ferritest]
async fn mocks_user_list(page: Arc<Page>) {
    let handler: RouteHandler = Arc::new(|route: Route| {
        route.fulfill(FulfillResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: br#"[{"id":1,"name":"Ada"},{"id":2,"name":"Grace"}]"#.to_vec(),
            content_type: Some("application/json".into()),
        });
    });
    page.route(UrlMatcher::glob("**/api/users")?, handler, None).await?;

    page.goto("https://app.example.com/users").await?;
    expect(&page.locator(".user-row")).to_have_count(2).await?;
}
```

## Block third-party trackers

```rust
let block: RouteHandler = Arc::new(|route: Route| {
    route.abort("blockedbyclient");
});
page.route(
    UrlMatcher::glob("**/{google-analytics,segment,mixpanel}.com/**")?,
    block,
    None,
).await?;
```

## Modify a request

```rust
use ferridriver::route::ContinueOverrides;

let modify: RouteHandler = Arc::new(|route: Route| {
    let mut headers: Vec<(String, String)> = route
        .request()
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    headers.push(("x-test-run".into(), "ci-12345".into()));
    route.continue_route(ContinueOverrides {
        url: None,
        method: None,
        headers: Some(headers),
        post_data: None,
    });
});
page.route(UrlMatcher::glob("**/api/**")?, modify, None).await?;
```

## Wait for a specific response

```rust
let response = page
    .wait_for_response(UrlMatcher::glob("**/api/checkout")?, Some(30_000))
    .await?;
assert_eq!(response.status(), 200);
let body: serde_json::Value = response.json().await?;
assert_eq!(body["order_id"], "abc-123");
```

## TypeScript

```ts
await page.route('**/api/users', (route) => {
  route.fulfill({
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
let handler: RouteHandler = Arc::new(|route: Route| {
    route.fulfill(FulfillResponse {
        status: 200,
        headers: vec![],
        body: b"{}".to_vec(),
        content_type: Some("application/json".into()),
    });
});
context.route(UrlMatcher::glob("**/api/**")?, handler, None).await?;
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
page.unroute(&UrlMatcher::glob("**/api/users")?).await?;
```

Or set up the route inside a test fixture so teardown removes it
automatically when the context closes.
