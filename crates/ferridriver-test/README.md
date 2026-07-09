# ferridriver-test

[![crates.io](https://img.shields.io/crates/v/ferridriver-test.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-test)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-test?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-test)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Playwright-compatible test runner for Rust. Parallel workers, DAG-resolved
fixtures, hooks, retries with flaky detection, auto-retrying assertions,
text and pixel-diff snapshots, Playwright-compatible traces, and a wide
set of reporters.

## Quick start

```rust
use ferridriver_test::prelude::*;

#[ferritest]
async fn loads_homepage(page: Arc<Page>) {
    page.goto("https://example.com").await?;
    expect(&page).to_have_title("Example Domain").await?;
}

#[ferritest(retries = 2, tag = "smoke", timeout = "30s")]
async fn login_flow(page: Arc<Page>) {
    page.goto("https://app.example.com/login").await?;
    page.locator("#email").fill("user@example.com").await?;
    page.locator("button[type=submit]").click().await?;
    expect(&page).to_have_url("/dashboard").await?;
}
```

Harness:

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
ferridriver-test = "0.3"
```

```bash
cargo test --test e2e
cargo test --test e2e -- --headed --backend webkit -j 1
```

## `#[ferritest]` attributes

```
retries = N              # per-test retry count
timeout = "30s"          # per-test timeout — duration string ("500ms", "30s", "5m")
tag = "smoke"            # tag for --tag filtering (repeatable)
skip                     # unconditional skip
skip = "firefox"         # conditional skip; condition is browser name, platform name,
                         # env var, "ci", or a "!" negation
skip = "firefox | flaky" # condition | reason
slow                     # 3x default timeout
slow = "ci"              # conditional slow
fixme                    # known broken (skipped, but reported separately from skip)
fixme = "webkit"         # conditional fixme
fail                     # expect failure: pass iff the body fails
fail = "linux"           # conditional expect-failure
only                     # isolate one test (--forbid-only catches strays in CI)
info = "JIRA-123"        # arbitrary metadata, attached to the test result
use_options = r#"{ ... }"# JSON overrides for launch / context options
```

## Parameterized tests

```rust
#[ferritest_each(data = [
    ("https://example.com",   "Example Domain"),
    ("https://rust-lang.org", "Rust Programming Language"),
])]
async fn title_check(page: Arc<Page>, url: &str, title: &str) {
    page.goto(url).await?;
    expect(&page).to_contain_title(title).await?;
}
```

Generates one test per row, named `title_check (<row values>)`.

## Built-in fixtures

| Name         | Scope  | Type                  |
|--------------|--------|-----------------------|
| `browser`    | Worker | `Arc<Browser>`        |
| `request`    | Worker | `Arc<HttpClient>`     |
| `context`    | Test   | `Arc<BrowserContext>` |
| `page`       | Test   | `Arc<Page>`           |
| `test_info`  | Test   | `Arc<TestInfo>`       |

Resolution walks Global → Worker → Test. Workers reuse the browser; each
test gets a fresh context and page. Register custom fixtures with the
`#[fixture(scope = "...")]` macro — `async fn name(ctx: TestContext) ->
ferridriver_test::Result<T>`, resolved via `ctx.get::<T>("name")`;
dependencies (built-in or custom) resolve lazily and the DAG is validated
at startup.

## Hooks

```rust
#[before_all]   async fn setup(ctx: TestContext) { ... }    // once per suite per worker
#[after_all]    async fn teardown(ctx: TestContext) { ... } // once per suite per worker (runs on failure)
#[before_each]  async fn auth(ctx: TestContext) { ... }     // before every test
#[after_each]   async fn dump(ctx: TestContext) { ... }     // after every test, even on failure
```

## Expect matchers (38)

All matchers support `.not`, `.with_timeout(Duration)`, `.with_message(&str)`,
`.soft()`. Auto-retry on the Playwright polling schedule
`[100, 250, 500, 1000, ...]` ms up to `expect_timeout` (default 5000 ms).

**Page (4):** `to_have_title`, `to_contain_title`, `to_have_url`, `to_contain_url`.

**Locator — visibility / state (10):** `to_be_visible`, `to_be_hidden`,
`to_be_enabled`, `to_be_disabled`, `to_be_checked`, `to_be_editable`,
`to_be_attached`, `to_be_empty`, `to_be_focused`, `to_be_in_viewport`.

**Locator — text / value (6):** `to_have_text`, `to_contain_text`,
`to_have_value`, `to_have_values`, `to_have_texts`, `to_contain_texts`.

**Locator — attributes (9):** `to_have_attribute`, `to_have_class`,
`to_contain_class`, `to_have_css`, `to_have_id`, `to_have_role`,
`to_have_accessible_name`, `to_have_accessible_description`,
`to_have_accessible_error_message`.

**Locator — other (5):** `to_have_js_property`, `to_have_count`,
`to_match_snapshot`, `to_have_screenshot`, `to_match_aria_snapshot`.

**Poll / satisfy (4):** `to_equal`, `to_satisfy`, `to_pass`,
`to_pass_with_options`.

## Reporters

Built-in reporter names (set via `[[test.reporter]] name = "..."` in
config, or `--reporter NAME[:OPTIONS]` on the `ferridriver bdd` CLI):

`terminal`, `progress`, `dot`, `json`, `junit`, `html`, `blob`, `allure`,
`github`, `rerun`, `messages` / `ndjson` (Cucumber Messages), `usage`,
`cucumber-json`, `empty`.

Multiple reporters can run simultaneously — events fan out via a broadcast
bus.

## CLI flags (after `--`)

Parsed by `parse_common_cli_args` (the after-`--` flag parser for
`#[ferritest]` harnesses):

```
--headless               Run the browser without a visible window
--backend NAME           cdp-pipe | cdp-raw | webkit | bidi
-j N, --workers N        Parallel workers
--retries N              Retry failed tests
--timeout MS             Per-test timeout
--global-timeout MS      Whole-run timeout
-g PATTERN, --grep ...   Filter by test name (regex, case-insensitive)
--tag NAME               Filter by tag
--list                   List tests without running
-u, --update-snapshots   Update snapshot files
--ignore-snapshots       Skip snapshot comparisons
--last-failed            Re-run only previously failed tests
--forbid-only            Fail if any #[ferritest(only)] is present
--max-failures N         Stop after N failures
--repeat-each N          Run each test N times
--fail-on-flaky-tests    Treat flaky passes as failures
--project NAME           Filter by project (repeatable)
--profile NAME           Apply a named [test.profiles.NAME] preset
```

Reporters, sharding, browser/project selection, and most other settings
are configured via `ferridriver.toml`; the `ferridriver bdd` subcommand
exposes its own clap flags (`--reporter`, `--shard`, …).

## Architecture

The `TestRunner::run()` pipeline is the single execution path for
`#[ferritest]`, `#[ferritest_each]`, BDD scenarios (via `ferridriver-bdd`),
and any external translator.

```
TestPlan
  └── filter (shard, grep, tag, only, last-failed, forbid-only)
  └── validate fixture DAG
  └── run global setup
  └── Dispatcher (MPMC work-stealing channel)
        ├── Worker 0   Browser
        ├── Worker 1   Browser
        └── ...        (concurrent launch via tokio::join!)
  └── collect results (retry failed tests via re-enqueue)
  └── run global teardown
  └── exit code
```

Workers launch browsers concurrently (not serially) — overlapping launches
save 80–100 ms per extra worker on warm machines.

## License

MIT OR Apache-2.0
