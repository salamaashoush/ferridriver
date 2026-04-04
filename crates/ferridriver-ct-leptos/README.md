# ferridriver-ct-leptos

Component testing for Leptos. Runs `cargo test` with a real browser -- your Leptos WASM components render in Chrome, assertions use the full ferridriver Page/Locator API.

## Setup

```toml
# Cargo.toml
[dev-dependencies]
ferridriver-ct-leptos = { path = "../ferridriver-ct-leptos" }

[[test]]
name = "components"
harness = false
```

## Writing Tests

```rust
use ferridriver_ct_leptos::prelude::*;

#[component_test]
async fn counter_increments(page: Page) {
    page.locator("#inc").click().await.unwrap();
    expect(&page.locator("#count")).to_have_text("1").await.unwrap();
}

#[component_test]
async fn counter_decrements(page: Page) {
    page.locator("#dec").click().await.unwrap();
    expect(&page.locator("#count")).to_have_text("-1").await.unwrap();
}

ferridriver_ct_leptos::main!();
```

Run with:

```sh
cargo test --test components
```

## How It Works

1. **`trunk build`** -- Compiles your Leptos app to WASM (cached, only rebuilds on changes)
2. **Static serve** -- Starts an HTTP server on a random port serving `dist/`
3. **Parallel runner** -- N workers, each with its own Chrome instance, pull tests from an MPMC queue
4. **Per-test isolation** -- Each test gets a fresh browser context + page, navigated to the CT server
5. **Full expect API** -- All 32 auto-retrying matchers from ferridriver-test

The `#[component_test]` macro registers tests via `inventory`. The `main!()` macro generates a `fn main()` that collects all registered tests and feeds them into `ferridriver_test::TestRunner`.

## Architecture

```
cargo test
  --> trunk build (WASM)
  --> ComponentServer::start(dist/)
  --> TestRunner::run(plan)
      --> Worker 0: Browser::launch() -> new_context() -> new_page() -> goto(server_url)
      --> Worker 1: Browser::launch() -> new_context() -> new_page() -> goto(server_url)
      --> Worker N: ...
  --> ComponentServer::stop()
```

## Performance

500 component tests in ~10s (8 workers, M1 Mac). The bottleneck is `trunk build`, not test execution -- subsequent runs with cached builds complete in seconds.

## Prerequisites

- `trunk` installed: `cargo install trunk`
- Chrome/Chromium available in PATH
- Your Leptos project must have a valid `index.html` for trunk
