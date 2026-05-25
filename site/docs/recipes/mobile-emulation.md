# Mobile emulation

ferridriver does not ship a device descriptor catalog. Configure the
emulation primitives directly: viewport, user agent, device scale
factor, mobile flag, touch flag, locale, timezone, geolocation.

## Per-test

```rust
use ferridriver_test::prelude::*;
use ferridriver::options::{ContextOptions, ViewportOption, ScreenSize};

#[ferritest]
async fn mobile_layout(ctx: TestContext) {
    let context = ctx.browser().await?.new_context_with_options(
        ContextOptions {
            viewport: ViewportOption::Size(ScreenSize { width: 390, height: 844 }),
            device_scale_factor: Some(3.0),
            is_mobile: Some(true),
            has_touch: Some(true),
            user_agent: Some(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) \
                 AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 \
                 Mobile/15E148 Safari/604.1".into()
            ),
            locale: Some("en-US".into()),
            ..Default::default()
        }
    ).await?;

    let page = context.new_page().await?;
    page.goto("https://example.com", None).await?;
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
ferridriver bdd --project iphone-15-pro tests/features/
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

let context = ctx.browser().await?.new_context_with_options(
    ContextOptions {
        geolocation: Some(Geolocation {
            latitude: 48.858844,
            longitude: 2.294351,
            accuracy: Some(20.0),
        }),
        permissions: Some(vec!["geolocation".into()]),
        ..Default::default()
    }
).await?;
```

## Timezone and locale

```rust
ContextOptions {
    timezone_id: Some("Europe/Paris".into()),
    locale: Some("fr-FR".into()),
    ..Default::default()
}
```

The page's `Intl` and `Date.now()` reflect both.

## Color scheme and contrast

```rust
ContextOptions {
    color_scheme: ferridriver::options::MediaOverride::Value("dark".into()),
    contrast: ferridriver::options::MediaOverride::Value("more".into()),
    reduced_motion: ferridriver::options::MediaOverride::Value("reduce".into()),
    ..Default::default()
}
```

Or per page after creation:

```rust
page.emulate_media(EmulateMediaOptions {
    color_scheme: Some("dark".into()),
    ..Default::default()
}).await?;
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
