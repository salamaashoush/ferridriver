# ferridriver-ct-dioxus

Component testing for Dioxus. Same architecture as `ferridriver-ct-leptos` but uses `dx build` instead of `trunk build`.

## Setup

```toml
# Cargo.toml
[dev-dependencies]
ferridriver-ct-dioxus = { path = "../ferridriver-ct-dioxus" }

[[test]]
name = "components"
harness = false
```

## Writing Tests

```rust
use ferridriver_ct_dioxus::prelude::*;

#[component_test]
async fn counter_increments(page: Page) -> Result<(), TestFailure> {
    page.locator("#inc").click().await?;
    expect(&page.locator("#count")).to_have_text("1").await?;
    Ok(())
}

#[component_test]
async fn form_submits(page: Page) -> Result<(), TestFailure> {
    page.locator("#name").fill("Alice").await?;
    page.locator("button[type=submit]").click().await?;
    expect(&page.locator("#result")).to_contain_text("Alice").await?;
    Ok(())
}

ferridriver_ct_dioxus::main!();
```

Run with:

```sh
cargo test --test components
```

## How It Works

1. **`dx build --platform web`** -- Compiles your Dioxus app to WASM
2. **Find output** -- Searches `target/dx/<app>/debug/web/public/` (dx 0.7) or `target/dx/<app>/public/` (dx 0.6) for `index.html`
3. **Static serve** -- Starts an HTTP server on a random port
4. **Parallel runner** -- N workers with isolated browser contexts, MPMC dispatch
5. **Full expect API** -- All 32 auto-retrying matchers from ferridriver-test

## Architecture

```
cargo test
  --> dx build --platform web
  --> find target/dx/*/public/index.html
  --> ComponentServer::start(public/)
  --> TestRunner::run(plan)
      --> Worker 0..N: Browser::launch() -> new_context() -> new_page() -> goto(url)
  --> ComponentServer::stop()
```

## Differences from Leptos Adapter

| | Leptos | Dioxus |
|---|---|---|
| Build tool | `trunk build` | `dx build --platform web` |
| Output dir | `dist/` | `target/dx/<app>/public/` |
| Install | `cargo install trunk` | `cargo install dioxus-cli` |
| Everything else | Same | Same |

## Prerequisites

- `dx` (dioxus-cli) installed: `cargo install dioxus-cli`
- Chrome/Chromium available in PATH
