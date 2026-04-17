# Rust tests

Write tests with `#[ferritest]`; generate a harness with `ferridriver_test::main!()`; run with `cargo test`.

## Project layout

```
my-project/
├── Cargo.toml
├── ferridriver.config.toml   # optional, auto-discovered
└── tests/
    ├── harness.rs            # main!() — one per test binary
    ├── homepage.rs
    └── login.rs
```

## Harness

```rust
// tests/harness.rs
mod homepage;
mod login;

ferridriver_test::main!();
```

```toml
# Cargo.toml
[[test]]
name = "e2e"
path = "tests/harness.rs"
harness = false

[dev-dependencies]
ferridriver-test = "0.1"
```

## Tests

Every `#[ferritest]` function takes a single `TestContext` argument. Name it whatever you like — the macro binds the context regardless of the type annotation. Use `ctx.page().await?` to get the pre-created `Arc<Page>` (plus `ctx.context()`, `ctx.browser()`, `ctx.test_info()`).

```rust
// tests/homepage.rs
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_homepage(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://example.com", None).await?;
    expect(&page).to_have_title("Example Domain").await?;
}

#[ferritest(retries = 2, tag = "smoke")]
async fn login_flow(ctx: TestContext) {
    let page = ctx.page().await?;
    page.goto("https://app.example.com/login", None).await?;
    page.locator("#email").fill("user@example.com").await?;
    page.locator("button[type=submit]").click().await?;
    expect(&page).to_have_url("https://app.example.com/dashboard").await?;
}
```

The generated wrapper returns `Result<(), TestFailure>` and propagates `?` — the body can use `?` freely on anything that converts into `TestFailure` (everything from ferridriver core via `String: From<&str>` and the provided `From<String>` impls).

## Run

```bash
cargo test --test e2e
cargo test --test e2e -- --headed --backend webkit -j 1
cargo test --test e2e -- -g smoke --retries 2
```

## `#[ferritest]` attributes

```
retries = N             # per-test retry count
timeout = "30s"          # per-test timeout (duration string: "500ms", "30s", "5m")
tag = "smoke"            # grouping tag for --tag filtering
skip                     # always skip
slow                     # triple the default timeout
fixme                    # mark as known broken
fail                     # pass if and only if the body fails
only                     # isolate (--forbid-only catches stray ones in CI)
info = "JIRA-123"        # arbitrary metadata attached to test result
use_options = ...        # per-test override of LaunchOptions / ContextOptions
```

Conditional forms like `skip = "linux"` or `fixme = "firefox | known bug"` are also supported (condition | reason).

## Parameterized tests

Use `#[ferritest_each(data = [ ... ])]` with a `(ctx: TestContext, input: T)` signature:

```rust
#[ferritest_each(data = [
    ("https://example.com", "Example Domain"),
    ("https://rust-lang.org", "Rust Programming Language"),
])]
async fn title_check(ctx: TestContext, case: (&str, &str)) {
    let page = ctx.page().await?;
    page.goto(case.0, None).await?;
    expect(&page).to_contain_title(case.1).await?;
}
```
