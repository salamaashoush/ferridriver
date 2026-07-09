# Mobile emulation

ferridriver does not ship a device descriptor catalog. Configure the
emulation primitives directly: viewport, user agent, device scale
factor, mobile flag, touch flag, locale, timezone, geolocation.

## Per-test

```rust
use ferridriver_test::prelude::*;
use ferridriver::options::ViewportOption;

#[ferritest]
async fn mobile_layout(browser: Arc<Browser>) {
    let context = browser.new_context()
        .viewport(ViewportOption::Size { width: 390, height: 844 })
        .device_scale_factor(3.0)
        .mobile(true)
        .has_touch(true)
        .user_agent(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
             AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 \
             Mobile/15E148 Safari/604.1",
        )
        .locale("en-US")
        .await?;

    let page = context.new_page().await?;
    page.goto("https://example.com").await?;
    expect(&page.locator(".mobile-nav")).to_be_visible().await?;
}
```

## Project-wide

```toml
[[test.projects]]
name = "iphone-15-pro"
[test.projects.browser]
browser = "webkit"
backend = "webkit"

[test.projects.browser.useOptions]
isMobile  = true
hasTouch  = true
userAgent = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"
deviceScaleFactor = 3.0
locale    = "en-US"

[test.projects.browser.viewport]
width  = 390
height = 844
```

```bash
cargo test --test e2e -- --project iphone-15-pro
```

## Touch interactions

When `hasTouch` is `true`, use the touch API (or `tap`) instead of
`click`:

```rust
page.touchscreen().tap(200.0, 400.0).await?;
page.locator("button.cta").tap().await?;
```

## Geolocation

```rust
use ferridriver::options::Geolocation;

let context = browser.new_context()
    .geolocation(Geolocation {
        latitude: 48.858844,
        longitude: 2.294351,
        accuracy: 20.0,
    })
    .permissions(vec!["geolocation".into()])
    .await?;
```

## Timezone and locale

```rust
let context = browser.new_context()
    .timezone_id("Europe/Paris")
    .locale("fr-FR")
    .await?;
```

The page's `Intl` and `Date.now()` reflect both.

## Color scheme and contrast

```rust
use ferridriver::options::MediaOverride;

let context = browser.new_context()
    .color_scheme(MediaOverride::Set("dark".into()))
    .contrast(MediaOverride::Set("more".into()))
    .reduced_motion(MediaOverride::Set("reduce".into()))
    .await?;
```

Or per page after creation:

```rust
use ferridriver::options::MediaOverride;

page.emulate_media()
    .color_scheme(MediaOverride::Set("dark".into()))
    .await?;
```

## TypeScript

```ts
const context = await browser.newContext({
  viewport: { width: 390, height: 844 },
  deviceScaleFactor: 3,
  isMobile: true,
  hasTouch: true,
  userAgent: 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) ...',
  locale: 'en-US',
  timezoneId: 'America/Los_Angeles',
  colorScheme: 'dark',
});
```
