# Cookies and storage

`BrowserContext` owns cookies, localStorage, sessionStorage, and
IndexedDB. Set them before the first navigation and they are present on
page load.

## Pre-seed a cookie

```rust
use ferridriver_test::prelude::*;
use ferridriver::backend::{CookieData, SameSite};

#[ferritest]
async fn opts_in_to_beta(ctx: TestContext) {
    let context = ctx.browser_context().await?;
    context.add_cookies(vec![CookieData {
        name: "feature_beta".into(),
        value: "1".into(),
        domain: ".example.com".into(),
        path: "/".into(),
        expires: None,
        http_only: false,
        secure: true,
        same_site: Some(SameSite::Lax),
        url: None,
    }]).await?;

    let page = ctx.page().await?;
    page.goto("https://app.example.com/", None).await?;
    expect(&page.locator(".beta-banner", None)).to_be_visible().await?;
}
```

## Read all cookies

```rust
let cookies = context.cookies().await?;
for c in cookies {
    println!("{}={}", c.name, c.value);
}
```

## Clear / delete

```rust
context.clear_cookies().await?;       // all
context.delete_cookie("session", Some(".example.com")).await?;
```

## Pre-seed localStorage

`storageState` is the most reliable path — JS-injected localStorage
before navigation can fight with the page's own bootstrap.

```rust
let state = serde_json::json!({
    "cookies": [],
    "origins": [{
        "origin": "https://app.example.com",
        "localStorage": [
            { "name": "feature_beta", "value": "1" },
            { "name": "preferred_locale", "value": "fr-FR" }
        ]
    }]
});

let bytes = serde_json::to_vec(&state).map_err(|e| e.to_string())?;
std::fs::write(".auth/seed.json", bytes).map_err(|e| e.to_string())?;
```

Then in config:

```toml
[test.browser.useOptions]
storageState = ".auth/seed.json"
```

## Read storage state

```rust
let page = ctx.page().await?;
page.goto("https://app.example.com/", None).await?;
let state = page.storage_state().await?;
// state is JSON: { cookies: [...], origins: [{origin, localStorage: [...]}] }
```

## sessionStorage via init script

`storageState` only restores cookies and localStorage. For
sessionStorage (or to clear specific keys), use an init script:

```rust
let context = ctx.browser_context().await?;
context.add_init_script(r#"
  window.sessionStorage.setItem('returning_visitor', '1');
  window.sessionStorage.removeItem('onboarding_step');
"#.into(), None).await?;

let page = ctx.page().await?;
page.goto("https://app.example.com/", None).await?;
```

## TypeScript

```ts
await context.addCookies([{
  name: 'feature_beta',
  value: '1',
  domain: '.example.com',
  path: '/',
  sameSite: 'Lax',
  secure: true,
  httpOnly: false,
}]);

const cookies = await context.cookies();

await context.clearCookies();

const state = await context.storageState();
await Bun.write('.auth/state.json', JSON.stringify(state));
```
