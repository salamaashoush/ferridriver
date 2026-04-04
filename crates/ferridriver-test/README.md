# ferridriver-test

Playwright-compatible test runner for Rust. Parallel execution across multiple browser instances with auto-retrying assertions, fixtures, hooks, reporters, snapshots, and tracing.

## Quick Start

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn basic_navigation(page: Page) {
    page.goto("https://example.com", None).await.unwrap();
    expect(&page).to_have_title("Example Domain").await.unwrap();
}

#[ferritest(retries = 2, tag = "smoke")]
async fn login_flow(page: Page) {
    page.goto("https://app.example.com/login", None).await.unwrap();
    page.locator("#email").fill("user@example.com").await.unwrap();
    page.locator("#password").fill("password").await.unwrap();
    page.locator("button[type=submit]").click().await.unwrap();
    expect(&page).to_have_url("https://app.example.com/dashboard").await.unwrap();
}
```

## Features

- **Parallel execution** -- N workers x N browsers, MPMC work-stealing dispatch
- **Serial suites** -- `SuiteMode::Serial` runs tests in order, skips rest on failure
- **Auto-retrying assertions** -- `expect()` polls with Playwright's interval pattern (100, 250, 500, 1000ms)
- **Fixtures** -- DAG-resolved, scoped (global/worker/test), injected via `FixturePool`
- **Hooks** -- `before_all`, `after_all`, `before_each`, `after_each` (per-suite, per-worker tracking)
- **Retries with flaky detection** -- failed tests re-dispatched, final status = `Flaky` if passed on retry
- **Reporters** -- terminal, HTML, JSON, JUnit XML (multiplexed via event bus)
- **Text snapshots** -- `assert_snapshot()` creates/diffs `.snap` files with unified diff
- **Screenshot snapshots** -- pixel-level PNG comparison with configurable threshold, diff image generation
- **Tracing** -- Playwright-compatible ZIP traces (`npx playwright show-trace trace.zip`)
- **Sharding** -- `--shard 1/4` for CI parallelism
- **Annotations** -- `skip`, `slow` (3x timeout), `fixme`, `fail` (expected failure inversion), `tag`
- **Soft assertions** -- `expect().soft()` collects errors without stopping the test
- **Config** -- `ferridriver.config.toml` / `.json`, env vars, CLI overrides (priority: CLI > env > file > defaults)

## Configuration

```toml
# ferridriver.config.toml
workers = 4
timeout = 30000
expect_timeout = 5000
retries = 1
fully_parallel = true

[browser]
backend = "cdp-pipe"
headless = true
```

## Performance

Benchmarked against Playwright Test on the same 50-test workload (navigate + click + assert):

| Runner | 50 tests | tests/sec |
|---|---|---|
| Playwright Test (4 workers) | ~2200ms | ~23/s |
| ferridriver-test (4 workers) | ~600ms | ~83/s |
| ferridriver-test (6 workers) | ~525ms | ~95/s |

~3.7x faster than Playwright. Overlapped browser launch saves ~80-100ms. Per-test overhead is dominated by CDP round-trips, not runner dispatch.

## Expect Matchers (32)

### Page (2)
| Matcher | Description |
|---|---|
| `to_have_title` | Page title matches string or regex |
| `to_have_url` | Page URL matches string or regex |

### Locator -- Visibility/State (10)
| Matcher | Description |
|---|---|
| `to_be_visible` | Element is visible |
| `to_be_hidden` | Element is hidden |
| `to_be_enabled` | Element is enabled |
| `to_be_disabled` | Element is disabled |
| `to_be_checked` | Checkbox/radio is checked |
| `to_be_editable` | Element is editable |
| `to_be_attached` | Element is in the DOM |
| `to_be_empty` | Element has no text content |
| `to_be_focused` | Element has focus |
| `to_be_in_viewport` | Element is within the viewport |

### Locator -- Text/Value (6)
| Matcher | Description |
|---|---|
| `to_have_text` | Text content matches exactly |
| `to_contain_text` | Text content contains substring |
| `to_have_value` | Input value matches |
| `to_have_values` | Multi-select values match array |
| `to_have_texts` | Multiple elements' text matches array |
| `to_contain_texts` | Multiple elements contain substrings |

### Locator -- Attributes (8)
| Matcher | Description |
|---|---|
| `to_have_attribute` | Attribute value matches |
| `to_have_class` | Class attribute matches |
| `to_contain_class` | Class list contains name |
| `to_have_css` | Computed CSS property matches |
| `to_have_id` | id attribute matches |
| `to_have_role` | ARIA role matches |
| `to_have_accessible_name` | Accessible name matches |
| `to_have_accessible_description` | Accessible description matches |

### Locator -- Other (6)
| Matcher | Description |
|---|---|
| `to_have_js_property` | JS property matches JSON value |
| `to_have_count` | Element count matches |
| `to_match_snapshot` | Text matches stored `.snap` file |
| `to_have_screenshot` | Screenshot matches stored PNG (pixel diff) |
| `to_match_aria_snapshot` | Accessibility tree matches YAML |
| `expect_poll().to_equal` | Poll async value until it equals expected |

All matchers support `.not()`, `.with_timeout()`, `.with_message()`, and `.soft()`.
