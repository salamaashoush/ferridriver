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

```toml
[test]
trace = "retain-on-failure"
```

Modes: `off`, `on`, `retain-on-failure`, `on-first-retry`.

## Reading a trace

The trace viewer ships inside the binary — no npm, no download, works
offline:

```bash
ferridriver trace view                    # the newest trace of the last run
ferridriver trace view path/to/trace.zip  # a specific one
ferridriver trace view --no-open --port 9323
```

`npx playwright show-trace` and [trace.playwright.dev] open the same
files: the format is Playwright's, version 8.

Without a browser — over ssh, in a CI log, from an agent — read it as
text:

```bash
ferridriver trace show                    # call tree, timings, failures
ferridriver trace show --errors           # only what went wrong
ferridriver trace show --json             # the whole model, for a script
ferridriver trace ls                      # what the last run recorded
```

```text
$ ferridriver trace show --errors
checkout > pays
chromium · darwin · ferridriver/0.5.0 · 24 actions · 2 pages · 4.1s
  x locator.click #submit  1.2s
      TimeoutError Timeout 1000ms exceeded
      waiting for locator('#submit')

network 14 requests, 1 failed
  500 POST   https://app.local/api/pay
```

[trace.playwright.dev]: https://trace.playwright.dev

## Manual tracing

```rust
context.tracing().start(TracingStartOptions {
    screenshots: true,
    snapshots: true,
    sources: true,
    ..Default::default()
}).await?;
page.goto("https://app.example.com").await?;
page.locator("button.cta").click().await?;
context.tracing().stop(TracingStopOptions { path: Some("trace.zip".into()) }).await?;
```

```ts
await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
await context.tracing.group('checkout');   // nests what follows in the viewer
await page.goto('https://app.example.com');
await context.tracing.groupEnd();
await context.tracing.stop({ path: 'trace.zip' });
```

`browserType.launch({ tracesDir })` decides where the loose trace files
are written while recording; `live: true` flushes them as they happen, so
a viewer can follow a recording that has not finished yet.

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
