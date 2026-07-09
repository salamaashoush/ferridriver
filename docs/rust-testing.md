# Writing E2E tests in Rust

ferridriver's test runner is not just a backend for the BDD and NAPI
surfaces — it is a first-class Rust E2E framework. This guide covers the
authoring surface: harness setup, tests, fixtures, hooks, assertions, and
runtime options.

## Harness setup

Tests live in a normal integration-test binary that replaces libtest with
the ferridriver runner. In `Cargo.toml`:

```toml
[dev-dependencies]
ferridriver-test = { path = "../ferridriver-test" }
tokio = { version = "1", features = ["rt-multi-thread"] }

[[test]]
name = "e2e"
harness = false
```

In `tests/e2e.rs`:

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_home_page(page: Arc<Page>) {
    page.goto("https://example.com").await?;
    expect(&page).to_have_title("Example Domain").await?;
}

ferridriver_test::main!();
```

`main!()` expands to a `main` that reads `ferridriver.toml` (auto-discovered
walking up from the working directory), applies CLI flags, discovers every
`#[ferritest]` in the binary, and runs them through the parallel worker pool.
A complete runnable example lives at `examples/rust-e2e`.

Under cargo-nextest the harness binary appears as a single opaque test
(nextest drives libtest binaries per-test; a `harness = false` binary is
one unit to it). Filter inside the harness instead: `--grep` /
`FERRITEST_GREP` select individual tests, and the harness's own workers
provide the parallelism.

Run it like any cargo test, passing runner flags after `--`:

```sh
cargo test --test e2e                                  # all tests
cargo test --test e2e -- --headless --backend webkit   # runner flags
cargo test --test e2e -- --grep login --workers 4
```

Or through the CLI, which forwards the common flags as `FERRITEST_*`
environment variables so they reach harness binaries even when other test
binaries run in the same invocation:

```sh
ferridriver test --headless --backend cdp-pipe --grep login
```

## Tests and fixture parameters

`#[ferritest]` functions declare what they need as parameters. Two forms:

- `name: Arc<T>` — resolves the fixture named `name` (built-in or custom)
  and injects it. The parameter name is the fixture name; the type is
  checked at resolution.
- `ctx: TestContext` — dynamic access to any fixture plus runner services.

Built-in fixtures:

| Parameter | Type | Scope |
|---|---|---|
| `page` | `Arc<Page>` | test — fresh page per test |
| `context` | `Arc<BrowserContext>` | test |
| `browser` | `Arc<Browser>` | worker — shared across the worker's tests |
| `request` | `Arc<HttpClient>` | worker — pre-configured with `baseURL` |
| `test_info` | `Arc<TestInfo>` | test — retry index, output dir, attachments |

```rust
#[ferritest]
async fn checkout(page: Arc<Page>, request: Arc<HttpClient>) {
    let seed = request.post("/api/seed").await?;
    page.goto("/checkout").await?;
    // ...
}
```

`TestContext` offers the same via getters (`ctx.page().await?`,
`ctx.request().await?`, `ctx.get::<T>("name").await?`) when a test needs
conditional or late resolution.

### Attribute arguments

```rust
#[ferritest(
    retries = 2,               // per-test retry override
    timeout = "30s",           // "30s" or "5000ms"
    tag = "smoke",             // repeatable
    skip = "backend == webkit | flaky compositor", // condition | reason
    slow,                      // 3x timeout (also: slow = "ci")
    fixme, fail, only,         // Playwright annotations
    info = "issue:PROJ-123",   // structured metadata
)]
```

Context overrides per test (Playwright's `test.use`), as structured keys:

```rust
#[ferritest(viewport = "390x844", is_mobile, has_touch, locale = "de-DE",
            color_scheme = "dark", timezone_id = "Europe/Berlin")]
async fn mobile_dark(page: Arc<Page>) { /* ... */ }
```

Supported keys: `viewport = "WxH"`, `locale`, `color_scheme`, `timezone_id`,
`user_agent`, `reduced_motion`, `forced_colors`, `service_workers`,
`storage_state`, `base_url`, `device_scale_factor`, and the boolean flags
`is_mobile`, `has_touch`, `offline`, `java_script_enabled`, `bypass_csp`,
`accept_downloads`, `ignore_https_errors`. Anything else (geolocation,
permissions, headers, credentials) goes through the raw escape hatch
`use_options = r#"{...}"#` (validated at compile time; structured keys win
on conflict).

### Parameterized tests

`#[ferritest_each]` expands one function into a test per data row. Fixture
parameters come first, then one parameter per tuple element. Every
`#[ferritest]` argument is accepted and applies to all rows.

```rust
#[ferritest_each(data = [
    ("admin", "admin@example.com"),
    ("guest", "guest@example.com"),
], names = ["admin login", "guest login"], tag = "auth", retries = 1)]
async fn login(page: Arc<Page>, role: &str, email: &str) {
    page.goto(&format!("/login?role={role}")).await?;
    page.fill("#email", email).await?;
}
```

`names` labels the generated tests (`login (admin login)`, ...); without
it the row values become the suffix.

## Custom fixtures

`#[fixture]` registers a scoped, dependency-injected fixture. Parameters
work exactly like test parameters — `Arc<T>` pulls another fixture by
parameter name, so dependencies are declared in the signature:

```rust
#[fixture(scope = "worker")]                    // "test" (default) | "worker" | "global"
async fn db(_ctx: TestContext) -> ferridriver_test::Result<DbHandle> {
    DbHandle::connect().await
}

#[fixture(scope = "test")]
async fn seeded_users(db: Arc<DbHandle>) -> ferridriver_test::Result<Vec<User>> {
    db.seed_users().await
}

#[ferritest]
async fn lists_users(page: Arc<Page>, seeded_users: Arc<Vec<User>>) {
    page.goto("/users").await?;
    expect(&page.locator("li")).to_have_count(seeded_users.len()).await?;
}
```

`#[fixture(auto)]` resolves the fixture for every test in scope without an
explicit parameter (Playwright's `auto: true`). `timeout = "10s"` bounds
setup.

Fixtures that need cleanup return `Fixture<T>` with an `on_teardown`;
teardowns run when the fixture's scope ends, in reverse setup order:

```rust
#[fixture(scope = "worker")]
async fn db(_ctx: TestContext) -> ferridriver_test::Result<Fixture<DbHandle>> {
    let db = DbHandle::connect().await?;
    Ok(Fixture::new(db).on_teardown(|db| async move { db.drop_schema().await; }))
}
```

## Suites and hooks

Modules are suites. `#[before_all]` / `#[after_all]` run once per suite per
worker; `#[before_each]` / `#[after_each]` wrap every test. Hook parameters
resolve fixtures the same way tests do.

```rust
#[ferritest_suite(mode = "serial")]   // serial: one worker, source order, stop on failure
mod payment_flow {
    use ferridriver_test::prelude::*;

    #[before_each]
    async fn login(page: Arc<Page>) {
        page.goto("/login").await?;
    }

    #[ferritest]
    async fn initiate(page: Arc<Page>) { /* ... */ }
}
```

## Driving the browser

Every option-taking operation returns a deferred action: await it directly
for defaults, or chain option setters first. No `None` arguments, no option
structs at call sites:

```rust
page.goto("https://example.com").await?;
page.goto(url).wait_until(LoadState::NetworkIdle).timeout(Duration::from_secs(10)).await?;

page.click("#submit").await?;
page.locator(".row").click().button(MouseButton::Right).position((8.0, 8.0)).await?;
locator.fill("hello").timeout(2_000u64).await?;

let png = page.screenshot().full_page(true).format(ScreenshotFormat::Jpeg).await?;
let ctx = browser.new_context().locale("de-DE").viewport(ViewportOption::Fixed(cfg)).await?;
```

Enumerated options are real enums (`LoadState`, `WaitState`,
`ScreenshotFormat`, `Role`, `MouseButton`, ...) with `From<&str>` so
string literals keep working; timeout/delay setters take ms (`u64`) or a
`Duration`. Bindings and code that already hold a parsed option struct
pass it wholesale with `.options(bag)` or `.maybe_options(maybe_bag)`.

Typed evaluate wraps the wire protocol with serde on both sides:

```rust
let count: u32 = page.eval("() => document.images.length").await?;
let ok: bool = page.eval_with("sel => !!document.querySelector(sel)", &"#app").await?;
let text: String = locator.eval("el => el.textContent").await?;
```

Event waits arm before the triggering action runs, so nothing races:

```rust
let (download, ()) = page.expect_download(|| page.click("#export")).await?;
let (msg, ()) = page.expect_console(|| page.click("button")).await?;
```

(`expect_dialog`, `expect_request`, `expect_response`, `expect_websocket`,
`expect_file_chooser`, `expect_page_error`, and the generic
`expect_event(name, action)` follow the same shape.)

## Assertions

`expect()` gives auto-retrying, Playwright-scheduled assertions. Subjects
are accepted in any borrowable form — `&page` (an `Arc<Page>` fixture),
`&locator`, temporaries like `&page.locator("h1")`:

```rust
expect(&page).to_have_title("Dashboard").await?;
expect(&page.locator("h1")).to_have_text("Welcome").await?;
expect(&locator).not().to_be_visible().await?;
expect(&locator).with_timeout(Duration::from_secs(10)).to_have_count(3).await?;
```

`expect_poll(|| async { ... })`, `to_pass`, value matchers
(`expect_value`), and screenshot/aria-snapshot matchers
(`to_have_screenshot`, `to_match_aria_snapshot`) are re-exported through
`ferridriver_test::expect`.

Plain `assert!` / `assert_eq!` / `panic!` are safe inside tests, fixtures,
and hooks: the worker catches the unwind and records it as that test's
failure (with screenshot-on-failure), like any other assertion error.

## Configuration

`ferridriver.toml`'s `[test]` section supplies defaults (workers, retries,
timeout, `expectTimeout` for the default `expect()` deadline, backend,
baseURL, reporters, viewport, storageState, webServer, projects, ...).
CLI flags after `--` override the file; `FERRITEST_*` environment
variables sit between the two. See `docs/` config reference for the full
key list.
