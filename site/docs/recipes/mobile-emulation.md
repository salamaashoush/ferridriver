# Mobile emulation

ferridriver does not ship a device descriptor catalog. Configure the
emulation primitives directly: viewport, user agent, device scale
factor, mobile flag, touch flag, locale, timezone, geolocation.

## Per-test

```rust
use ferridriver_test::prelude::*;
use ferridriver::options::{BrowserContextOptions, ViewportOption};

#[ferritest]
async fn mobile_layout(ctx: TestContext) {
    let context = ctx.browser().await?.new_context(Some(
        BrowserContextOptions {
            viewport: ViewportOption::Size { width: 390, height: 844 },
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
    ));

    let page = context.new_page().await?;
    page.goto("https://example.com", None).await?;
    expect(&page.locator(".mobile-nav", None)).to_be_visible().await?;
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
page.locator("button.cta", None).tap(None).await?;
```

## Geolocation

```rust
use ferridriver::options::{BrowserContextOptions, Geolocation};

let context = ctx.browser().await?.new_context(Some(
    BrowserContextOptions {
        geolocation: Some(Geolocation {
            latitude: 48.858844,
            longitude: 2.294351,
            accuracy: 20.0,
        }),
        permissions: Some(vec!["geolocation".into()]),
        ..Default::default()
    }
));
```

## Timezone and locale

```rust
use ferridriver::options::BrowserContextOptions;

BrowserContextOptions {
    timezone_id: Some("Europe/Paris".into()),
    locale: Some("fr-FR".into()),
    ..Default::default()
};
```

The page's `Intl` and `Date.now()` reflect both.

## Color scheme and contrast

```rust
use ferridriver::options::{BrowserContextOptions, MediaOverride};

BrowserContextOptions {
    color_scheme: MediaOverride::Set("dark".into()),
    contrast: MediaOverride::Set("more".into()),
    reduced_motion: MediaOverride::Set("reduce".into()),
    ..Default::default()
};
```

Or per page after creation:

```rust
use ferridriver::options::{EmulateMediaOptions, MediaOverride};

page.emulate_media(&EmulateMediaOptions {
    color_scheme: MediaOverride::Set("dark".into()),
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
