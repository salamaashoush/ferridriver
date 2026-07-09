# Screenshots and traces

## Page screenshots

```rust
use ferridriver::options::ScreenshotFormat;

let png = page.screenshot().await?;
std::fs::write("home.png", png).map_err(|e| e.to_string())?;

// Full page (scrolling capture)
let png = page.screenshot().full_page(true).await?;

// JPEG with quality
let jpg = page.screenshot()
    .format(ScreenshotFormat::Jpeg)
    .quality(80)
    .await?;
```

## Element screenshots

```rust
let png = page.locator(".chart").screenshot().await?;
```

## Masking sensitive regions

Overlay a solid color over selected elements before snapshotting:

```rust
let png = page.screenshot()
    .full_page(true)
    .mask([
        page.locator(".user-email"),
        page.locator(".credit-card"),
    ])
    .mask_color("#FF00FF")
    .await?;
```

## Disable animations for stable captures

```rust
use ferridriver::options::{AnimationsMode, CaretMode};

let png = page.screenshot()
    .animations(AnimationsMode::Disabled)
    .caret(CaretMode::Hide)
    .await?;
```

## Snapshot assertions

Stored baseline; failures emit a diff image next to the snapshot.

```rust
use ferridriver_test::prelude::*;
use ferridriver_test::expect::LocatorSnapshotMatchers;

#[ferritest]
async fn dashboard_snapshot(page: Arc<Page>) {
    page.goto("https://app.example.com/dashboard").await?;
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
use ferridriver_test::prelude::*;
use ferridriver_test::expect::PageSnapshotMatchers;

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
page.goto("https://app.example.com").await?;
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

// Locator.screenshot() returns the bytes (no path option) — write them yourself.
const chart = await page.locator('.chart').screenshot();
await Bun.write('chart.png', chart);
```
