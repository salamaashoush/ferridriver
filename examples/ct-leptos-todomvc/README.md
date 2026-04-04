# Leptos TodoMVC — Component Testing Example

Full TodoMVC app with 15 component tests using ferridriver's Playwright-style API.

## Quick Start

```bash
# Prerequisites
cargo install trunk

# Run tests
cargo test -p ct-leptos-todomvc --test todomvc
```

## What You See

```
Running 15 test(s) with 16 worker(s)

  ✓ edit_todo_on_double_click (244ms)
  ✓ toggle_todo_complete (205ms)
  ✓ add_multiple_todos (219ms)
  ✓ filter_active (220ms)
  ...

  15 test(s): 15 passed (531ms)
```

## How It Works

1. `trunk build` compiles the Leptos WASM app (cached after first run)
2. Static server serves `dist/` on a random port
3. ferridriver's parallel test runner launches N browsers
4. Each test gets a fresh page navigated to the app
5. Auto-retrying `expect` assertions with 5s timeout

## Writing Tests

```rust
use ferridriver_ct_leptos::prelude::*;

#[component_test]
async fn add_todo(page: Page) -> Result<(), TestFailure> {
    // Interact with the component
    page.locator("#new-todo").fill("Buy milk").await?;
    page.locator("#new-todo").press("Enter").await?;

    // Auto-retrying assertions (polls for up to 5 seconds)
    expect(&page.locator(".todo-list li")).to_have_count(1).await?;
    expect(&page.locator(".todo-list li label")).to_have_text("Buy milk").await?;
    Ok(())
}

// Required: custom harness entry point
ferridriver_ct_leptos::main!();
```

### Cargo.toml

```toml
[dev-dependencies]
ferridriver-ct-leptos = { path = "../../crates/ferridriver-ct-leptos" }
ferridriver = { path = "../../crates/ferridriver" }
ferridriver-test = { path = "../../crates/ferridriver-test" }
tokio = { version = "1", features = ["full"] }

[[test]]
name = "my_tests"
harness = false
```

## API Reference

### `#[component_test]`

Registers an async test function. Receives a `Page` already navigated to the app.

Two styles:

```rust
// Idiomatic: return Result, use ? for clean error propagation
#[component_test]
async fn my_test(page: Page) -> Result<(), TestFailure> {
    page.locator("button").click().await?;
    expect(&page.locator("#result")).to_have_text("done").await?;
    Ok(())
}

// Simple: unwrap/assert, panics on failure
#[component_test]
async fn simple_test(page: Page) {
    page.locator("button").click().await.unwrap();
    assert_eq!(page.locator("#result").text_content().await.unwrap().unwrap(), "done");
}
```

### `expect` Assertions

Auto-retrying assertions matching Playwright's API:

```rust
// Visibility
expect(&page.locator("#elem")).to_be_visible().await?;
expect(&page.locator("#elem")).to_be_hidden().await?;

// Text
expect(&page.locator("h1")).to_have_text("Hello").await?;
expect(&page.locator("p")).to_contain_text("world").await?;

// Count
expect(&page.locator(".item")).to_have_count(5).await?;

// Value
expect(&page.locator("input")).to_have_value("hello").await?;

// Attributes
expect(&page.locator("button")).to_have_attribute("disabled", "true").await?;
expect(&page.locator("div")).to_have_class("active").await?;

// Negation
expect(&page.locator("#elem")).not().to_be_visible().await?;

// Custom timeout
expect(&page.locator(".slow"))
    .with_timeout(Duration::from_secs(10))
    .to_be_visible().await?;
```

### Page API

```rust
// Navigation
page.goto("https://example.com", None).await?;

// Locators
page.locator("css selector").click().await?;
page.locator("#input").fill("text").await?;
page.locator("#input").press("Enter").await?;
page.locator("label").dblclick().await?;

// Reading
let text = page.locator("h1").text_content().await?;
let value = page.locator("input").input_value().await?;
let count = page.locator(".item").count().await?;
```

## Architecture

```
cargo test
    │
    ▼
#[component_test] macro → registers tests via inventory
    │
    ▼
ferridriver_ct_leptos::main!() → custom harness
    │
    ├── trunk build (cached, ~500ms)
    ├── ComponentServer serves dist/ on random port
    ├── ferridriver TestRunner with N workers
    │   ├── Worker 0: Browser → Page → test_fn
    │   ├── Worker 1: Browser → Page → test_fn
    │   └── Worker N: Browser → Page → test_fn
    └── MPMC dispatcher (natural load balancing)
```

## Performance

| Tests | Time | Throughput |
|-------|------|-----------|
| 15 | 531ms | 28 tests/sec |
| 500 | 10.1s | 49 tests/sec |

One `trunk build` (cached). N browsers in parallel. Fresh page per test (~20ms).
