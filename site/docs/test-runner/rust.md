# Rust tests

Write tests with `#[ferritest]`; generate a harness with
`ferridriver_test::main!()`; run with `cargo test`.

## Project layout

```
my-project/
├── Cargo.toml
├── ferridriver.toml         # optional, auto-discovered
└── tests/
    ├── harness.rs           # main!() — one per test binary
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
ferridriver-test = "0.4"
```

## Tests

Every `#[ferritest]` function takes a single `TestContext` argument. The
macro binds the context regardless of the type annotation — name it
whatever you like.

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
    expect(&page).to_have_url("/dashboard").await?;
}
```

Use `ctx.page()` for the pre-created `Arc<Page>`. Also available:
`ctx.browser_context()`, `ctx.browser()`, `ctx.test_info()`.

The generated wrapper returns `Result<(), TestFailure>` and propagates
`?` — the body can use `?` freely on anything that converts into
`TestFailure`.

## Run

```bash
cargo test --test e2e
cargo test --test e2e -- --backend webkit -j 1
cargo test --test e2e -- -g smoke --retries 2
```

Tests run headed by default; pass `--headless` to opt into headless mode.

## `#[ferritest]` attributes

```
retries = N             # per-test retry count
timeout = "30s"         # per-test timeout — "500ms", "30s", "5m", ...
tag     = "smoke"       # tag for --tag filtering (repeatable)

skip                    # unconditional skip
skip    = "firefox"     # conditional — browser name, platform name,
                        #   env var name, "ci", or "!" prefix
skip    = "firefox | flaky on Firefox"  # condition | reason
slow                    # 3x default timeout
slow    = "ci"          # conditional slow
fixme                   # known broken (skipped, reported separately)
fixme   = "webkit"      # conditional fixme
fail                    # expected failure (pass iff body fails)
fail    = "linux"       # conditional expected failure
only                    # isolate one test (--forbid-only catches strays in CI)
info    = "JIRA-123"    # arbitrary metadata
use_options = r#"{...}"#  # JSON overrides for launch / context options
```

## Parameterized tests

```rust
#[ferritest_each(data = [
    ("https://example.com",   "Example Domain"),
    ("https://rust-lang.org", "Rust Programming Language"),
])]
async fn title_check(ctx: TestContext, case: (&str, &str)) {
    let page = ctx.page().await?;
    page.goto(case.0, None).await?;
    expect(&page).to_contain_title(case.1).await?;
}
```

Generates one test per row, named `title_check (<row values>)`. The
first parameter is the test context; subsequent parameters receive the
tuple element(s).
