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

## Expect Matchers (38)

### Page (4)
| Matcher | Description |
|---|---|
| `to_have_title` | Page title matches string or regex |
| `to_contain_title` | Page title contains substring |
| `to_have_url` | Page URL matches string or regex |
| `to_contain_url` | Page URL contains substring |

### Locator — Visibility / State (10)
| Matcher | Description |
|---|---|
| `to_be_visible` | Element is visible |
| `to_be_hidden` | Element is hidden |
| `to_be_enabled` | Element is enabled |
| `to_be_disabled` | Element is disabled |
| `to_be_checked` | Checkbox / radio is checked |
| `to_be_editable` | Element is editable |
| `to_be_attached` | Element is in the DOM |
| `to_be_empty` | Element has no text content |
| `to_be_focused` | Element has focus |
| `to_be_in_viewport` | Element is within the viewport |

### Locator — Text / Value (6)
| Matcher | Description |
|---|---|
| `to_have_text` | Text content matches exactly |
| `to_contain_text` | Text content contains substring |
| `to_have_value` | Input value matches |
| `to_have_values` | Multi-select values match array |
| `to_have_texts` | Multiple elements' text matches array |
| `to_contain_texts` | Multiple elements contain substrings |

### Locator — Attributes (9)
| Matcher | Description |
|---|---|
| `to_have_attribute` | Attribute value matches |
| `to_have_class` | Class attribute matches |
| `to_contain_class` | Class list contains name |
| `to_have_css` | Computed CSS property matches |
| `to_have_id` | `id` attribute matches |
| `to_have_role` | ARIA role matches |
| `to_have_accessible_name` | Accessible name matches |
| `to_have_accessible_description` | Accessible description matches |
| `to_have_accessible_error_message` | Accessible error message matches |

### Locator — Other (5)
| Matcher | Description |
|---|---|
| `to_have_js_property` | JS property matches JSON value |
| `to_have_count` | Element count matches |
| `to_match_snapshot` | Text matches stored `.snap` file |
| `to_have_screenshot` | Screenshot matches stored PNG (pixel diff) |
| `to_match_aria_snapshot` | Accessibility tree matches YAML |

### Poll / satisfy (4)
| Matcher | Description |
|---|---|
| `to_equal` | Polled value equals expected |
| `to_satisfy` | Polled value passes a user predicate |
| `to_pass` | Run an async closure until it succeeds |
| `to_pass_with_options` | `to_pass` with custom `intervals` / `timeout` |

All matchers support `.not()`, `.with_timeout()`, `.with_message()`, and `.soft()`.

## Architecture

### Execution Pipeline

```
TestPlan (suites + tests)
    |
TestRunner.run()
    |
    +-- filter (shard, grep, tag)
    +-- validate fixture DAG
    +-- run global setup
    +-- Dispatcher (MPMC unbounded channel)
    |       |
    |   +---+---+---+
    |   |   |   |   |
    |  W0  W1  W2  W3   (workers, each with own Browser)
    |   |   |   |   |
    |   +---+---+---+
    |       |
    +-- collect results (retry failed tests)
    +-- run global teardown
    +-- return exit code
```

Workers launch browsers concurrently (overlapped, not sequential) for ~80-100ms savings.

### Worker Per-Test Lifecycle

```
beforeAll (SuiteHookFn, once per suite per worker)
  |
  create BrowserContext + Page (isolated per test)
  |
  inject fixtures: browser, context, page, test_info
  |
  beforeEach (HookFn, receives FixturePool + Arc<TestInfo>)
  |
  test body (with timeout; x3 for @slow)
  |
  afterEach (always runs, even on failure)
  |
  screenshot on failure (if configured)
  |
  close context + teardown fixtures (LIFO)
  |
  determine status (check @fail inversion, soft errors)
  |
afterAll (SuiteHookFn, on worker shutdown)
```

### Fixture System

Dependency-injected fixtures with three scopes and automatic LIFO teardown.

```
Global pool (shared across all workers)
  |
  Worker pool (one per worker, inherits global)
    |
    Test pool (one per test, inherits worker)
```

**Resolution:** `pool.get::<T>("name")` walks the scope chain, resolves dependencies recursively, caches values, registers teardown. DAG validated at startup.

**Built-in fixtures:**

| Name | Scope | Type |
|------|-------|------|
| `browser` | Worker | `Arc<Browser>` |
| `context` | Test | `Arc<ContextRef>` |
| `page` | Test | `Arc<Page>` |
| `test_info` | Test | `Arc<TestInfo>` |

### Hook Types

```rust
// Suite-scoped: once per suite per worker, no test context
type SuiteHookFn = Arc<dyn Fn(FixturePool) -> BoxFuture<Result<()>>>;

// Test-scoped: per test, has test metadata and step API
type HookFn = Arc<dyn Fn(FixturePool, Arc<TestInfo>) -> BoxFuture<Result<()>>>;
```

### Reporter Event Pipeline

Workers emit events via `EventBus` (async broadcast). A spawned task fans events to all reporters.

```
RunStarted -> WorkerStarted -> TestStarted -> StepStarted* -> StepFinished*
-> TestFinished -> WorkerFinished -> RunFinished
```

Step events (`StepStarted`/`StepFinished`) are boxed to keep the enum small. They carry optional metadata (e.g., BDD keyword info) for domain-specific rendering.

### Step Tracking API

Tests can emit structured steps for live reporter rendering:

```rust
let handle = test_info.begin_step("Click login", StepCategory::TestStep).await;
// ... do work ...
handle.end(None).await;  // None = success, Some(msg) = failure

// Nested steps
let parent = test_info.begin_step("Fill form", StepCategory::TestStep).await;
let child = test_info.begin_child_step("Email", StepCategory::TestStep, &parent.step_id).await;
child.end(None).await;
parent.end(None).await;
```

Categories: `TestStep`, `Expect`, `Fixture`, `Hook`, `PwApi`

### Retry and Flaky Detection

Failed tests are re-dispatched to the shared queue (any worker can pick them up). `RetryPolicy::final_status()` determines the outcome:

- All attempts passed: `Passed`
- Last attempt passed, prior failed: `Flaky`
- Last attempt failed: `Failed`
- All skipped: `Skipped`

### Dispatcher

- **Parallel suites:** Each test enqueued as `WorkItem::Single` to shared MPMC channel. Natural load balancing -- fast workers pull more.
- **Serial suites:** All tests batched as `WorkItem::Serial`. One worker gets the batch, runs tests in order, skips rest on first failure.
- **Retry:** Re-enqueued as `WorkItem::Single` (any worker can pick it up).
