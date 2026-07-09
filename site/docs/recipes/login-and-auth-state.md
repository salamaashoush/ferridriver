# Login and saved auth state

Sign in once, dump the storage state, and reuse it across every test
that needs an authenticated session. No repeated login flows, no flaky
form-fill races.

## Capture the state once

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn save_auth(page: Arc<Page>) {
    page.goto("https://app.example.com/login").await?;
    page.locator("#email").fill("user@example.com").await?;
    page.locator("#password").fill("secret").await?;
    page.locator("button[type=submit]").click().await?;
    expect(&page).to_have_url("/dashboard").await?;

    let state = page.storage_state().await?;
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(".auth/admin.json", bytes).map_err(|e| e.to_string())?;
}
```

Run this once before the main suite — wire it as a
[global setup project](/test-runner/config) so CI does not skip it.

## Reuse it everywhere

```toml
# ferridriver.toml
[test]
storageState = ".auth/admin.json"
```

Every `BrowserContext` created from then on starts with the saved
cookies and localStorage. The first navigation in every test arrives
already-authenticated.

Per-project override (matrix runs):

```toml
[[test.projects]]
name = "authed"
[test.projects.browser.useOptions]
storageState = ".auth/admin.json"
```

## TypeScript

```ts
import { chromium } from '@ferridriver/node';

const browser = await chromium().launch();
const context = await browser.newContext();
const page = await context.newPage();

await page.goto('https://app.example.com/login');
await page.getByLabel('Email').fill('user@example.com');
await page.getByLabel('Password').fill('secret');
await page.getByRole('button', { name: 'Sign in' }).click();
await page.waitForUrl('/dashboard');

const state = await context.storageState();
await Bun.write('.auth/admin.json', JSON.stringify(state));

await browser.close();
```

Then load it on subsequent runs:

```ts
const context = await browser.newContext({
  storageState: '.auth/admin.json',
});
```

## Multiple roles

One state file per role:

```
.auth/
  admin.json
  editor.json
  viewer.json
```

```toml
[[test.projects]]
name = "admin"
[test.projects.browser.useOptions]
storageState = ".auth/admin.json"

[[test.projects]]
name = "editor"
[test.projects.browser.useOptions]
storageState = ".auth/editor.json"
```

```bash
cargo test --test e2e -- --project admin
cargo test --test e2e -- --project editor
```

## Invalidation

When the auth state goes stale (cookie expiry, password rotation), the
first authenticated test will redirect to `/login`. Detect with a guard
in `before_each`:

```rust
#[before_each]
async fn ensure_authed(page: Arc<Page>) {
    page.goto("https://app.example.com/").await?;
    if page.url().contains("/login") {
        panic!("auth state expired — re-run save_auth");
    }
}
```

## Why not just log in per test

A login flow costs ~1.5 s on a fast site, ~5 s on a slow one. Saved
state costs zero. On a 200-test suite at 4 workers the savings are 5–25
minutes per run. The state file is also more deterministic — no race
against an async login form.
