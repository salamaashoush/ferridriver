# Screenshots and traces

## Page screenshots

```rust
use ferridriver::options::ScreenshotOptions;

let png = page.screenshot(ScreenshotOptions::default()).await?;
std::fs::write("home.png", png)?;

// Full page (scrolling capture)
let png = page.screenshot(ScreenshotOptions {
    full_page: Some(true),
    ..Default::default()
}).await?;

// JPEG with quality
let jpg = page.screenshot(ScreenshotOptions {
    format: Some("jpeg".into()),
    quality: Some(80),
    ..Default::default()
}).await?;
```

## Element screenshots

```rust
let png = page.locator(".chart").screenshot(Default::default()).await?;
```

## Masking sensitive regions

Overlay a solid color over selected elements before snapshotting:

```rust
let png = page.screenshot(ScreenshotOptions {
    full_page: Some(true),
    mask: vec![".user-email".into(), ".credit-card".into()],
    mask_color: Some("#FF00FF".into()),
    ..Default::default()
}).await?;
```

## Disable animations for stable captures

```rust
let png = page.screenshot(ScreenshotOptions {
    animations: Some("disabled".into()),
    caret: Some("hide".into()),
    ..Default::default()
}).await?;
```

## Snapshot assertions

Stored baseline; failures emit a diff image next to the snapshot.

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn dashboard_snapshot(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://app.example.com/dashboard", None).await?;
    expect(&page.locator(".main")).to_have_screenshot("dashboard.png").await?;
}
```

First run writes the baseline. Subsequent runs compare. Update with:

```bash
cargo test --test e2e -- --update-snapshots
```

## ARIA / accessibility snapshots

YAML representation of the accessibility tree — readable, deterministic,
and great for catching unintended a11y regressions.

```rust
expect(&page).to_match_aria_snapshot(r#"
- banner:
  - link "Dashboard"
  - link "Settings"
- heading "Welcome, Ada" [level=1]
- button "Sign out"
"#).await?;
```

## Playwright-compatible traces

Recorded in `[test].trace` mode and dropped into the output directory.
Open with the Playwright trace viewer:

```toml
[test]
trace = "retain-on-failure"
```

```bash
npx playwright show-trace test-results/login-flow/trace.zip
```

Modes: `off`, `on`, `retain-on-failure`, `on-first-retry`.

## Manual tracing

```rust
page.start_tracing().await?;
page.goto("https://app.example.com", None).await?;
page.locator("button.cta").click().await?;
page.stop_tracing().await?;
// Output goes to launchOptions.traces_dir
```

## Video recording

Per-context:

```toml
[test.browser.useOptions.recordVideo]
dir  = "test-results/videos"
size = { width = 1280, height = 720 }
```

Modes:

```toml
[test.video]
mode = "retain-on-failure"   # off | on | retain-on-failure
```

Requires `ffmpeg` on `PATH` at runtime.

## TypeScript

```ts
await page.screenshot({ path: 'home.png', fullPage: true });
await page.locator('.chart').screenshot({ path: 'chart.png' });
```
