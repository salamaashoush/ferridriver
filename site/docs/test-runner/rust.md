# Rust tests

Write tests with `#[ferritest]`; generate a harness with
`ferridriver_test::main!()`; run with `cargo test`. A complete runnable
project lives at `examples/rust-e2e` in the repository.

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
ferridriver-test = "0.5"
```

## Tests and fixture parameters

`#[ferritest]` functions declare what they need as parameters: `Arc<T>`
parameters resolve the fixture named after the parameter, and a
`TestContext` parameter gives dynamic access (`ctx.get::<T>("name")`).

```rust
// tests/homepage.rs
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_homepage(page: Arc<Page>) {
    page.goto("https://example.com").await?;
    expect(&page).to_have_title("Example Domain").await?;
}

#[ferritest(retries = 2, tag = "smoke")]
async fn login_flow(page: Arc<Page>, request: Arc<HttpClient>) {
    request.post("/api/seed").await?;
    page.goto("https://app.example.com/login").await?;
    page.locator("#email").fill("user@example.com").await?;
    page.locator("button[type=submit]").click().await?;
    expect(&page).to_have_url("/dashboard").await?;
}
```

Built-in fixtures: `page: Arc<Page>` (fresh per test),
`context: Arc<BrowserContext>`, `browser: Arc<Browser>` (worker-shared),
`request: Arc<HttpClient>` (worker-shared, picks up `baseURL`),
`test_info: Arc<TestInfo>`.

The generated wrapper returns `Result<(), TestFailure>` and propagates
`?` — the body can use `?` freely on anything that converts into
`TestFailure`. Plain `assert!` / `assert_eq!` / panics are caught and
reported as that test's failure, with the panic's backtrace attached.

## Driving the browser

Every option-taking operation is a deferred action: await it directly for
defaults, or chain option setters first — no `None` arguments, no option
structs at call sites.

```rust
page.goto(url).wait_until(LoadState::NetworkIdle).timeout(Duration::from_secs(10)).await?;
page.get_by_role("button").name("Save").click().await?;
locator.click().button(MouseButton::Right).position((8.0, 8.0)).await?;
let png = page.screenshot().full_page(true).await?;

// Typed evaluate: serde on both sides.
let count: u32 = page.eval("() => document.images.length").await?;

// Event waits arm before the action runs — no subscribe race.
let (download, ()) = page.expect_download(|| page.click("#export")).await?;
```

## Custom fixtures

`#[fixture]` registers a scoped, dependency-injected fixture; teardown is
declared on the returned value:

```rust
#[fixture(scope = "worker")]
async fn db(_ctx: TestContext) -> ferridriver_test::Result<Fixture<DbHandle>> {
    let db = DbHandle::connect().await?;
    Ok(Fixture::new(db).on_teardown(|db| async move { db.drop_schema().await; }))
}

#[ferritest]
async fn lists_users(page: Arc<Page>, db: Arc<DbHandle>) { /* ... */ }
```

## Run

```bash
cargo test --test e2e
cargo test --test e2e -- --backend webkit -j 1
cargo test --test e2e -- -g smoke --retries 2
ferridriver test --headless --grep smoke     # forwards flags as FERRITEST_* env vars
```

Tests run headed by default; pass `--headless` to opt into headless mode.
Under cargo-nextest a `harness = false` binary appears as a single test;
filter inside the harness with `--grep` / `FERRITEST_GREP` instead.

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
```

Context overrides use structured keys (Playwright's `test.use`):

```
viewport = "390x844"    locale = "de-DE"       color_scheme = "dark"
timezone_id = "..."     user_agent = "..."     device_scale_factor = 2.0
is_mobile  has_touch  offline  java_script_enabled = false  bypass_csp
accept_downloads  ignore_https_errors
use_options = r#"{...}"#   # raw-JSON escape hatch for the rest
```

## Parameterized tests

```rust
#[ferritest_each(data = [
    ("https://example.com",   "Example Domain"),
    ("https://rust-lang.org", "Rust Programming Language"),
], names = ["example.com", "rust-lang.org"])]
async fn title_check(page: Arc<Page>, url: &str, title: &str) {
    page.goto(url).await?;
    expect(&page).to_contain_title(title).await?;
}
```

Generates one test per row — named after the `names` entry, or the row
values when `names` is omitted. Fixture parameters come first; the
remaining parameters receive the tuple elements. Every `#[ferritest]`
attribute is accepted and applies to all rows.
